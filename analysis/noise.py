"""Step 2 - how noisy are the measurements, and where are more runs worth it?

- ``precision.csv``: per unit and metric, the run-to-run CV, the 95 % CI half-width
  (relative to the mean, Student t like ``runs_summary.csv``) and the runs needed for ±5 %.
- ``rerun_candidates.csv``: per scenario and metric, pairs of OSes adjacent in the ranking
  whose median CIs overlap: their order is not established, and an estimate of the runs
  that would separate them. Re-running only these is much cheaper than re-running all.
- ``validity.csv``: units to treat with care (saturated client/server, CPU from the guest,
  fewer runs than the others, failure not recovered within the test).
"""
from __future__ import annotations

import math
from typing import Dict, List

import numpy as np
import pandas as pd
from scipy import stats

from analysis import config

TARGET_PRECISION = 0.05
MAX_RUNS = 200


def runs_needed(sd: float, mean: float, done: int, precision: float = TARGET_PRECISION) -> float:
    if not (sd > 0) or abs(mean) < 1e-12:
        return float(done) if sd == 0 else math.nan
    target = precision * abs(mean)
    for n in range(max(2, 3), MAX_RUNS + 1):
        if stats.t.ppf(0.975, n - 1) * sd / math.sqrt(n) <= target:
            return float(n)
    return math.inf


def precision_table(experiments: pd.DataFrame, metrics: List[str]) -> pd.DataFrame:
    rows = []
    for _, unit in experiments.iterrows():
        for name in metrics:
            n = unit.get(f"{name}_n")
            if pd.isna(n) or n < 2:
                continue
            mean, sd = unit.get(f"{name}_mean"), unit.get(f"{name}_sd")
            half = stats.t.ppf(0.975, int(n) - 1) * sd / math.sqrt(n)
            rows.append(
                {
                    "unit": unit["unit"],
                    "os": unit["os"],
                    "scenario": unit["scenario"],
                    "metric": name,
                    "n_runs": int(n),
                    "mean": mean,
                    "cv_pct": unit.get(f"{name}_cv_pct"),
                    "ci95_half_width_pct": half / abs(mean) * 100 if abs(mean) > 1e-12 else math.nan,
                    "runs_needed_5pct": runs_needed(sd, mean, int(n)),
                }
            )
    return pd.DataFrame(rows)


def precision_summary(precision: pd.DataFrame) -> pd.DataFrame:
    if precision.empty:
        return precision
    grouped = precision.groupby("metric")
    out = pd.DataFrame(
        {
            "units": grouped.size(),
            "median_cv_pct": grouped["cv_pct"].median(),
            "p90_cv_pct": grouped["cv_pct"].quantile(0.9),
            "share_within_5pct": grouped["ci95_half_width_pct"].apply(lambda s: float((s <= TARGET_PRECISION * 100).mean())),
            "median_runs_needed": grouped["runs_needed_5pct"].apply(lambda s: float(np.median(s.replace(np.inf, MAX_RUNS + 1)))),
        }
    ).reset_index()
    return out.sort_values("median_cv_pct", ascending=False)


def rerun_candidates(experiments: pd.DataFrame, metrics: List[str]) -> pd.DataFrame:
    """OS pairs adjacent in a scenario's ranking whose bootstrap CIs of the median overlap."""
    rows = []
    for name in metrics:
        spec = config.metric(name)
        if spec is None or name not in experiments.columns:
            continue
        for scenario, group in experiments.dropna(subset=[name]).groupby("scenario"):
            if group["os"].nunique() < 2:
                continue
            ordered = group.sort_values(name, ascending=not spec.higher_is_better)
            records = ordered.to_dict("records")
            for better, worse in zip(records, records[1:]):
                lo_b, hi_b = better.get(f"{name}_ci_lo"), better.get(f"{name}_ci_hi")
                lo_w, hi_w = worse.get(f"{name}_ci_lo"), worse.get(f"{name}_ci_hi")
                if any(pd.isna(v) for v in (lo_b, hi_b, lo_w, hi_w)):
                    continue
                if hi_w < lo_b or hi_b < lo_w:
                    continue  # separated
                diff = abs(better[name] - worse[name])
                sd_b = better.get(f"{name}_sd", math.nan)
                sd_w = worse.get(f"{name}_sd", math.nan)
                needed = math.nan
                if diff > 0 and not (pd.isna(sd_b) or pd.isna(sd_w)):
                    needed = math.ceil((1.96 ** 2) * (sd_b ** 2 + sd_w ** 2) / diff ** 2)
                    needed = float(min(max(needed, int(better.get("n_runs", 2))), MAX_RUNS))
                elif diff == 0:
                    needed = math.inf
                rows.append(
                    {
                        "metric": name,
                        "scenario": scenario,
                        "os_ahead": better["os"],
                        "os_behind": worse["os"],
                        "median_ahead": better[name],
                        "median_behind": worse[name],
                        "relative_gap_pct": diff / abs(worse[name]) * 100 if abs(worse[name]) > 1e-12 else math.nan,
                        "runs_done": int(min(better.get("n_runs", 0), worse.get("n_runs", 0))),
                        "runs_to_separate": needed,
                    }
                )
    out = pd.DataFrame(rows)
    if not out.empty:
        out = out.sort_values(["metric", "relative_gap_pct"], ascending=[True, False])
    return out


def validity_table(experiments: pd.DataFrame) -> pd.DataFrame:
    if experiments.empty:
        return experiments
    max_runs = experiments.groupby("scenario")["n_runs"].transform("max")
    flags: Dict[str, pd.Series] = {
        "endpoint_saturated": experiments.get("endpoint_saturated", pd.Series(0.0, index=experiments.index)).fillna(0) > 0,
        "cpu_from_guest": experiments.get("cpu_from_guest", pd.Series(0.0, index=experiments.index)).fillna(0) > 0,
        "fewer_runs": experiments["n_runs"] < max_runs,
        "failure_not_recovered": (experiments["failures"] == "failures") & (experiments.get("recovered", pd.Series(1.0, index=experiments.index)).fillna(1) < 1),
    }
    frame = pd.DataFrame(flags)
    frame.insert(0, "unit", experiments["unit"])
    frame.insert(1, "n_runs", experiments["n_runs"])
    return frame[frame.drop(columns=["unit", "n_runs"]).any(axis=1)]
