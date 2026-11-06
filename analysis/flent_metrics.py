"""Reduces one Flent result file (one run of one test) to one value per metric.

Read directly from the ``.flent.gz`` JSON (no ``flent`` call needed):
- ``x_values`` / ``results``: series resampled by Flent on a common grid (0.2 s);
- ``raw_values``: the actual ping samples, with their timestamps (used for the latency
  percentiles, since the resampled series interpolate between samples);
- ``metadata.T0``: start of the test, origin of ``x_values``.

The load phase is where ``TCP totals`` has values (RRUL starts its TCP flows 5 s after
the pings and stops them 5 s before the end). Pings before the load phase give the idle
latency.
"""
from __future__ import annotations

import gzip
import json
import math
import re
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

import numpy as np

from analysis import config

ACTIVE_PHASE_SERIES = "TCP totals"
ICMP_SERIES = "Ping (ms) ICMP"
UDP_EF_SERIES = "Ping (ms) UDP EF"
UDP_BE_SERIES = "Ping (ms) UDP BE"
AGGREGATE_FLOW_SUFFIXES = ("sum", "avg")

VOIP_SUBSERIES = {
    "rtt": "voip_rtt_ms",
    "induced_delay": "voip_induced_delay_ms",
    "jitter": "voip_jitter_ms",
    "loss": "voip_loss",
}

# Seconds dropped at both ends of the load phase for the within-run stability metric
STABILITY_TRIM_S = 2.0


def load_flent(path: Path) -> dict:
    opener = gzip.open if str(path).endswith(".gz") else open
    with opener(path, "rt", encoding="utf-8") as file:
        return json.load(file)


def parse_time(value) -> Optional[float]:
    """RFC 3339 / ISO timestamp (``2026-10-30T23:01:55.742040Z``) as epoch seconds."""
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    text = str(value).strip().replace("Z", "+00:00")
    try:
        return datetime.fromisoformat(text).timestamp()
    except ValueError:
        # Python 3.9 only accepts 0, 3 or 6 fractional digits
        match = re.match(r"^(.*\.)(\d+)(.*)$", text)
        if match:
            head, digits, rest = match.group(1)[:-1], match.group(2), match.group(3)
            try:
                return datetime.fromisoformat(f"{head}.{(digits + '000000')[:6]}{rest}").timestamp()
            except ValueError:
                return None
        return None


def _series(data: dict, name: str) -> Tuple[np.ndarray, np.ndarray]:
    """(x, y) of a resampled series, missing values as NaN."""
    x = np.asarray(data.get("x_values", []), dtype=float)
    values = data.get("results", {}).get(name)
    if values is None:
        return x, np.full(len(x), np.nan)
    y = np.array([np.nan if v is None else float(v) for v in values], dtype=float)
    return x[: len(y)], y[: len(x)]


def active_window(data: dict) -> Tuple[float, float]:
    """Start and end of the load phase, in seconds since T0."""
    x, y = _series(data, ACTIVE_PHASE_SERIES)
    present = np.flatnonzero(~np.isnan(y))
    if len(x) == 0:
        return 0.0, 0.0
    if len(present) == 0:
        return float(x[0]), float(x[-1])
    return float(x[present[0]]), float(x[present[-1]])


def _raw_samples(data: dict, name: str, t0: Optional[float]) -> Optional[Tuple[np.ndarray, np.ndarray]]:
    """Raw samples of a series as (seconds since T0, value), or None if not available."""
    raw = data.get("raw_values", {}).get(name)
    if not raw or t0 is None:
        return None
    times, values = [], []
    for sample in raw:
        if not isinstance(sample, dict):
            continue
        t, v = sample.get("t"), sample.get("val")
        if t is None or v is None:
            continue
        try:
            times.append(float(t) - t0)
            values.append(float(v))
        except (TypeError, ValueError):
            continue
    if not values:
        return None
    return np.asarray(times), np.asarray(values)


def _samples(data: dict, name: str, t0: Optional[float]) -> Tuple[np.ndarray, np.ndarray]:
    raw = _raw_samples(data, name, t0)
    if raw is not None:
        return raw
    x, y = _series(data, name)
    keep = ~np.isnan(y)
    return x[keep], y[keep]


def _mean_in(x: np.ndarray, y: np.ndarray, start: float, end: float) -> float:
    selected = y[(x >= start) & (x <= end) & ~np.isnan(y)]
    return float(np.mean(selected)) if len(selected) else math.nan


def _quantile_in(x: np.ndarray, y: np.ndarray, start: float, end: float, q: float) -> float:
    selected = y[(x >= start) & (x <= end) & ~np.isnan(y)]
    return float(np.quantile(selected, q)) if len(selected) else math.nan


def jain_index(values: Sequence[float]) -> float:
    """Jain's fairness index: 1 = all flows equal, 1/n = one flow takes everything."""
    values = np.asarray([v for v in values if v is not None and not math.isnan(v)], dtype=float)
    if len(values) == 0 or np.sum(values ** 2) == 0:
        return math.nan
    return float(np.sum(values) ** 2 / (len(values) * np.sum(values ** 2)))


def failure_metrics(data: dict, failure_x: float) -> Dict[str, float]:
    """Impact of a link failure happening at ``failure_x`` seconds since T0, measured on
    the total TCP throughput.

    - ``outage_s``: time spent below 10 % of the pre-failure throughput;
    - ``recovery_s``: time from the failure until the throughput is back above 90 % of the
      pre-failure level for 2 s (censored at the end of the test, see ``recovered``);
    - ``failure_throughput_loss_pct``: share of the throughput lost in the 10 s after.
    """
    x, y = _series(data, ACTIVE_PHASE_SERIES)
    start, end = active_window(data)
    in_load = (x >= start) & (x <= end)
    # Inside the load phase, a missing value means no data went through
    y = np.where(in_load & np.isnan(y), 0.0, y)

    before = y[(x >= failure_x - config.PRE_FAILURE_WINDOW_S) & (x < failure_x) & in_load]
    before = before[~np.isnan(before)]
    if len(before) == 0 or failure_x >= end:
        return {}
    reference = float(np.median(before))
    if reference <= 0:
        return {}

    after = (x >= failure_x) & in_load
    xa, ya = x[after], y[after]
    if len(xa) == 0:
        return {}
    step = float(np.median(np.diff(x))) if len(x) > 1 else 0.2

    outage = float(np.sum(ya < config.OUTAGE_LEVEL * reference) * step)

    hold = max(1, int(round(2.0 / step)))
    good = ya >= config.RECOVERY_LEVEL * reference
    recovery, recovered = float(xa[-1] - failure_x), False
    if good.all():
        recovery, recovered = 0.0, True
    else:
        first_bad = int(np.argmax(~good))
        for i in range(first_bad, len(good) - hold + 1):
            if good[i: i + hold].all():
                recovery, recovered = float(xa[i] - failure_x), True
                break

    window = (xa <= failure_x + config.PRE_FAILURE_WINDOW_S)
    loss = (1.0 - float(np.mean(ya[window])) / reference) * 100.0 if window.any() else math.nan

    return {
        "outage_s": outage,
        "recovery_s": recovery,
        "recovered": float(recovered),
        "failure_throughput_loss_pct": loss,
    }


def run_metrics(data: dict, failure_offsets_s: Sequence[float] = ()) -> Dict[str, float]:
    """All the metrics of one run. ``failure_offsets_s``: link failures, in seconds since
    the start of the test (only the first one is measured)."""
    t0 = parse_time(data.get("metadata", {}).get("T0"))
    start, end = active_window(data)
    results = data.get("results", {})
    metrics: Dict[str, float] = {"load_start_s": start, "load_end_s": end}

    for name, column in (("tcp_download_mbps", "TCP download sum"), ("tcp_upload_mbps", "TCP upload sum"), ("tcp_total_mbps", ACTIVE_PHASE_SERIES)):
        if column in results:
            x, y = _series(data, column)
            metrics[name] = _mean_in(x, y, start, end)

    if ACTIVE_PHASE_SERIES in results:
        x, y = _series(data, ACTIVE_PHASE_SERIES)
        selected = y[(x >= start + STABILITY_TRIM_S) & (x <= end - STABILITY_TRIM_S) & ~np.isnan(y)]
        if len(selected) > 1 and np.mean(selected) > 0:
            metrics["tcp_total_cv_pct"] = float(np.std(selected, ddof=1) / np.mean(selected) * 100.0)

    flows = []
    for column in results:
        parts = column.split()
        if len(parts) == 3 and parts[0] == "TCP" and parts[1] in ("download", "upload") and parts[2] not in AGGREGATE_FLOW_SUFFIXES:
            x, y = _series(data, column)
            flows.append(_mean_in(x, y, start, end))
    if len(flows) > 1:
        metrics["jain_fairness"] = jain_index(flows)

    if ICMP_SERIES in results:
        x, y = _samples(data, ICMP_SERIES, t0)
        metrics["icmp_latency_mean_ms"] = _mean_in(x, y, start, end)
        metrics["icmp_latency_median_ms"] = _quantile_in(x, y, start, end, 0.50)
        metrics["icmp_latency_p95_ms"] = _quantile_in(x, y, start, end, 0.95)
        metrics["icmp_latency_p99_ms"] = _quantile_in(x, y, start, end, 0.99)
        if start > 0:
            idle = _quantile_in(x, y, -math.inf, start - 0.5, 0.50)
            metrics["icmp_idle_median_ms"] = idle
            metrics["latency_induced_ms"] = metrics["icmp_latency_median_ms"] - idle

    ef = be = math.nan
    if UDP_EF_SERIES in results:
        x, y = _samples(data, UDP_EF_SERIES, t0)
        ef = metrics["udp_ef_latency_ms"] = _quantile_in(x, y, start, end, 0.50)
    if UDP_BE_SERIES in results:
        x, y = _samples(data, UDP_BE_SERIES, t0)
        be = metrics["udp_be_latency_ms"] = _quantile_in(x, y, start, end, 0.50)
    if not math.isnan(ef) and not math.isnan(be) and be > 0:
        metrics["ef_be_latency_ratio"] = ef / be

    metrics.update(_voip_metrics(data, start, end, ACTIVE_PHASE_SERIES in results))

    if failure_offsets_s:
        metrics.update(failure_metrics(data, float(failure_offsets_s[0])))

    return {k: v for k, v in metrics.items() if v is not None and not (isinstance(v, float) and math.isnan(v))}


def _voip_metrics(data: dict, start: float, end: float, has_load: bool) -> Dict[str, float]:
    """VoIP metrics, from ``VoIP::<sub>`` series or from the sub-values of the raw VoIP samples.
    The one-way ``delay`` is ignored: it depends on the clock offset between the machines."""
    metrics: Dict[str, float] = {}
    if not has_load:
        start, end = -math.inf, math.inf
    for column in data.get("results", {}):
        if "VoIP" not in column or "::" not in column:
            continue
        sub = column.split("::", 1)[1].strip().lower()
        if sub in VOIP_SUBSERIES:
            x, y = _series(data, column)
            metrics[VOIP_SUBSERIES[sub]] = _mean_in(x, y, start, end)

    if metrics:
        return metrics

    t0 = parse_time(data.get("metadata", {}).get("T0"))
    for column, raw in data.get("raw_values", {}).items():
        if "VoIP" not in column or not raw or t0 is None:
            continue
        collected: Dict[str, List[float]] = {}
        for sample in raw:
            if not isinstance(sample, dict) or sample.get("t") is None:
                continue
            t = float(sample["t"]) - t0
            if not (start <= t <= end):
                continue
            for sub, name in VOIP_SUBSERIES.items():
                value = sample.get(sub)
                if isinstance(value, (int, float)) and math.isfinite(value):
                    collected.setdefault(name, []).append(float(value))
        for name, values in collected.items():
            metrics.setdefault(name, float(np.mean(values)))
    return metrics


def describe(data: dict) -> Dict[str, str]:
    metadata = data.get("metadata", {})
    return {
        "flent_test": str(metadata.get("NAME", "")),
        "flent_title": str(metadata.get("TITLE", "")),
        "flent_t0": str(metadata.get("T0", "")),
    }
