"""Per-router resources of one run, over the steady state of the load phase.

Sources (same as ``src/utils/results/resources.rs``, but kept run by run instead of being
averaged over the runs):
- ``run_N/host_resources/<node>.csv``: host monitor, one line per second
  (``guest_pct``, ``total_pct``... normalised to the VM's vCPUs: 100 % = all vCPUs busy);
- ``run_N/<router>.log``: guest-side readings (``TRACE rx: total free used cpu``);
- ``run_N/experiment.log``: "Experiment start" / "Experiment end", the time origin.

CPU percentages are turned into cores (``pct / 100 * vcpus``), the only unit that can be
compared between resource levels: 20 % of 2 vCPUs is not the load of 20 % of 8 vCPUs.
"""
from __future__ import annotations

import csv
import math
import re
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set, Tuple

import numpy as np

from analysis import config
from analysis.flent_metrics import parse_time

EXPERIMENT_LOG = "experiment.log"
HOST_RESOURCES_DIR = "host_resources"
START_MARKER = "INFO experiment: Experiment start"
END_MARKER = "INFO experiment: Experiment end"
ENDPOINT_PATTERN = re.compile(r"^(client|server)", re.IGNORECASE)
# The busiest thread of a client or server above this: the endpoint, not the routers, may
# limit the throughput
ENDPOINT_SATURATION_PCT = 95.0


def normalize_node(name: str) -> str:
    """``AS1 Router 1`` and ``AS1_Router_1`` are the same node."""
    return re.sub(r"[^A-Za-z0-9]+", "_", name).strip("_")


def read_text(path: Path) -> str:
    return path.read_bytes().decode("utf-8", errors="replace")


def experiment_window(run_dir: Path) -> Optional[Tuple[float, float]]:
    """(start, end) epoch seconds from ``experiment.log``: last start and last end markers."""
    path = run_dir / EXPERIMENT_LOG
    if not path.is_file():
        return None
    start = end = None
    for line in read_text(path).splitlines():
        if START_MARKER in line or END_MARKER in line:
            stamp = next((t for t in (parse_time(tok) for tok in line.split()[:3]) if t is not None), None)
            if stamp is None:
                continue
            if START_MARKER in line:
                start = stamp
            else:
                end = stamp
    if start is None or end is None or end < start:
        return None
    return start, end


def read_host_csv(path: Path, origin: float) -> Dict[str, np.ndarray]:
    """Columns of a host monitor file as float arrays, with ``t`` in seconds since ``origin``."""
    columns: Dict[str, List[float]] = {}
    with open(path, newline="", encoding="utf-8", errors="replace") as file:
        reader = csv.DictReader(file)
        for row in reader:
            t = parse_time(row.get("timestamp"))
            if t is None:
                continue
            columns.setdefault("t", []).append(t - origin)
            for key, value in row.items():
                if key in (None, "timestamp", "busiest_thread"):
                    continue
                try:
                    number = float(value) if value not in (None, "") else math.nan
                except ValueError:
                    number = math.nan
                columns.setdefault(key, []).append(number if math.isfinite(number) else math.nan)
    length = len(columns.get("t", []))
    return {k: np.asarray(v[:length], dtype=float) for k, v in columns.items() if len(v) >= length}


def read_guest_log(path: Path, origin: float) -> Optional[Dict[str, np.ndarray]]:
    """Guest-side readings: ``t``, ``mem_used_mib``, ``cpu_pct``. None if the file holds none."""
    times, used, cpu = [], [], []
    for line in read_text(path).splitlines():
        if "TRACE rx:" not in line:
            continue
        head, payload = line.split("TRACE rx:", 1)
        stamp = next((t for t in (parse_time(tok) for tok in head.split()) if t is not None), None)
        fields = payload.split()[:4]
        if stamp is None or len(fields) < 4:
            continue
        try:
            total, _free, used_mib, cpu_pct = (float(f) for f in fields)
        except ValueError:
            continue
        if not (total > 0 and 0 <= used_mib <= total):
            continue
        times.append(stamp - origin)
        used.append(used_mib)
        cpu.append(cpu_pct if 0 <= cpu_pct <= 100 else math.nan)
    if not times:
        return None
    return {"t": np.asarray(times), "mem_used_mib": np.asarray(used), "cpu_pct": np.asarray(cpu)}


def _median(t: np.ndarray, values: Optional[np.ndarray], start: float, end: float) -> float:
    if values is None:
        return math.nan
    selected = values[(t >= start) & (t <= end)]
    selected = selected[~np.isnan(selected)]
    return float(np.median(selected)) if len(selected) else math.nan


def _mean(t: np.ndarray, values: Optional[np.ndarray], start: float, end: float) -> float:
    if values is None:
        return math.nan
    selected = values[(t >= start) & (t <= end)]
    selected = selected[~np.isnan(selected)]
    return float(np.mean(selected)) if len(selected) else math.nan


def node_metrics(
    host: Optional[Dict[str, np.ndarray]],
    guest: Optional[Dict[str, np.ndarray]],
    load: Tuple[float, float],
    idle_end: float,
    default_vcpus: Optional[int],
) -> Dict[str, float]:
    """Steady-state resources of one node. ``load``: steady-state window, ``idle_end``: end of
    the idle period before the load (both in seconds since the experiment start)."""
    start, end = load
    metrics: Dict[str, float] = {}
    if host is not None and "t" in host:
        t = host["t"]
        vcpus = _median(t, host.get("vcpu"), -math.inf, math.inf)
        if math.isnan(vcpus):
            vcpus = float(default_vcpus) if default_vcpus else math.nan
        metrics["vcpus"] = vcpus
        guest_pct = _median(t, host.get("guest_pct"), start, end)
        total_pct = _median(t, host.get("total_pct"), start, end)
        metrics["guest_cores"] = guest_pct / 100.0 * vcpus
        metrics["host_cores"] = total_pct / 100.0 * vcpus
        metrics["idle_guest_cores"] = _median(t, host.get("guest_pct"), -math.inf, idle_end) / 100.0 * vcpus
        metrics["busiest_thread_pct"] = _median(t, host.get("busiest_thread_pct"), start, end)
        metrics["vm_tx_pps"] = _median(t, host.get("vm_tx_pps"), start, end)
        metrics["ns_per_packet"] = _median(t, host.get("ns_per_packet"), start, end)
        metrics["drop_pps"] = _mean(t, host.get("drop_pps"), start, end)
        metrics["busy_vhost_threads"] = _median(t, host.get("busy_vhost_threads"), start, end)
        metrics["host_rss_mib"] = _median(t, host.get("rss_anon_mib"), start, end)
    if guest is not None:
        t = guest["t"]
        idle = _median(t, guest["mem_used_mib"], -math.inf, idle_end)
        loaded = _median(t, guest["mem_used_mib"], start, end)
        metrics["mem_idle_mib"] = idle
        metrics["mem_load_mib"] = loaded
        metrics["mem_delta_mib"] = loaded - idle
        metrics["guest_reported_cpu_pct"] = _median(t, guest["cpu_pct"], start, end)
        if "guest_cores" not in metrics and default_vcpus:
            # No host monitor (old results): guest-side CPU, not comparable between OSes
            metrics["guest_cores"] = metrics["guest_reported_cpu_pct"] / 100.0 * default_vcpus
            metrics["cpu_from_guest"] = 1.0
    return {k: v for k, v in metrics.items() if not (isinstance(v, float) and math.isnan(v))}


def is_active(metrics: Dict[str, float]) -> bool:
    return metrics.get("guest_cores", 0.0) >= config.ACTIVE_ROUTER_MIN_CORES or metrics.get("vm_tx_pps", 0.0) >= config.ACTIVE_ROUTER_MIN_PPS


def run_resources(
    run_dir: Path,
    load_window_s: Tuple[float, float],
    routers: Optional[Set[str]],
    default_vcpus: Optional[int],
) -> Tuple[List[Dict[str, object]], Dict[str, float]]:
    """Per-node rows and router aggregates of one run.

    ``load_window_s``: load phase in seconds since the experiment start (before trimming).
    ``routers``: normalised router names (from the experiment file); if None, routers are the
    nodes with a guest-side log, or failing that the nodes not named Client*/Server*.
    """
    window = experiment_window(run_dir)
    if window is None:
        return [], {}
    origin = window[0]
    load_start, load_end = load_window_s
    steady = (load_start + config.STEADY_STATE_TRIM_S, load_end - config.STEADY_STATE_TRIM_S)
    if steady[1] <= steady[0]:
        steady = (load_start, load_end)
    idle_end = load_start - 1.0

    hosts: Dict[str, Dict[str, np.ndarray]] = {}
    host_dir = run_dir / HOST_RESOURCES_DIR
    if host_dir.is_dir():
        for path in sorted(host_dir.glob("*.csv")):
            if path.name == "numa_placement.csv":
                continue
            try:
                hosts[normalize_node(path.stem)] = read_host_csv(path, origin)
            except (OSError, csv.Error):
                continue

    guests: Dict[str, Dict[str, np.ndarray]] = {}
    for path in sorted(run_dir.glob("*.log")):
        if path.name == EXPERIMENT_LOG:
            continue
        samples = read_guest_log(path, origin)
        if samples is not None:
            guests[normalize_node(path.stem)] = samples

    nodes = set(hosts) | set(guests)
    if routers is None:
        routers = set(guests) or {n for n in nodes if not ENDPOINT_PATTERN.match(n)}

    rows: List[Dict[str, object]] = []
    for node in sorted(nodes):
        metrics = node_metrics(hosts.get(node), guests.get(node), steady, idle_end, default_vcpus)
        role = "router" if node in routers else "endpoint"
        row: Dict[str, object] = {"node": node, "role": role, **metrics}
        row["active"] = bool(is_active(metrics)) if role == "router" else None
        rows.append(row)

    return rows, aggregate_routers(rows)


def _values(rows: Iterable[Dict[str, object]], key: str) -> List[float]:
    out = []
    for row in rows:
        value = row.get(key)
        if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value):
            out.append(float(value))
    return out


def aggregate_routers(rows: List[Dict[str, object]]) -> Dict[str, float]:
    """Topology-level resources. Routers that carry no traffic are not averaged in: in a
    redundant topology they would hide the load of the path actually used."""
    routers = [r for r in rows if r["role"] == "router"]
    active = [r for r in routers if r.get("active")]
    endpoints = [r for r in rows if r["role"] == "endpoint"]
    out: Dict[str, float] = {"routers": float(len(routers)), "active_routers": float(len(active))}

    def put(name: str, values: List[float], how) -> None:
        if values:
            out[name] = float(how(values))

    put("routers_guest_cores", _values(routers, "guest_cores"), np.sum)
    put("routers_host_cores", _values(routers, "host_cores"), np.sum)
    put("max_router_guest_cores", _values(routers, "guest_cores"), np.max)
    put("max_busiest_thread_pct", _values(routers, "busiest_thread_pct"), np.max)
    put("ns_per_packet", _values(active, "ns_per_packet"), np.median)
    put("drop_pps", _values(routers, "drop_pps"), np.sum)
    put("routers_mem_idle_mib", _values(routers, "mem_idle_mib"), np.sum)
    put("mean_router_mem_idle_mib", _values(routers, "mem_idle_mib"), np.mean)
    put("max_router_mem_delta_mib", _values(routers, "mem_delta_mib"), np.max)
    put("routers_idle_guest_cores", _values(routers, "idle_guest_cores"), np.sum)
    put("endpoints_max_busiest_thread_pct", _values(endpoints, "busiest_thread_pct"), np.max)
    if out.get("routers_guest_cores", 0) > 0 and "routers_host_cores" in out:
        out["emulation_overhead"] = out["routers_host_cores"] / out["routers_guest_cores"]
    if "endpoints_max_busiest_thread_pct" in out:
        out["endpoint_saturated"] = float(out["endpoints_max_busiest_thread_pct"] >= ENDPOINT_SATURATION_PCT)
    if any(r.get("cpu_from_guest") for r in routers):
        out["cpu_from_guest"] = 1.0
    return out
