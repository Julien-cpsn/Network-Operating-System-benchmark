"""Builds the tidy tables every analysis works on.

- ``runs.csv``: one row per experiment x test x run: the factors, the OS architecture,
  and every metric of that run (throughput, latency, fairness, failure, router resources).
- ``routers.csv``: one row per experiment x test x run x node (per-router resources).
- ``experiments.csv``: one row per experiment x test (a "unit"): median of each metric over
  the runs, its bootstrap 95 % confidence interval, its coefficient of variation.

Layout read (written by ``run``):
```text
experimentation/results/<topology>,<os>,<test batch>,<resources>,<nic>,<routing>,<failures>/
    run_N/experiment.log
    run_N/<router>.log
    run_N/host_resources/<node>.csv
    run_N/<test name>/*.flent.gz
experimentation/experiments/**/*.json   (fire times, failure events, node roles; optional)
```
"""
from __future__ import annotations

import json
import logging
import math
import os
import re
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np
import pandas as pd

from analysis import config
from analysis.flent_metrics import describe, load_flent, run_metrics
from analysis.resource_metrics import normalize_node, run_resources

try:  # Python >= 3.11
    import tomllib  # type: ignore
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

LOG = logging.getLogger("analysis.dataset")
RUN_PREFIX = "run_"


# --------------------------------------------------------------------------- metadata


def load_experiment_index(experiments_dir: Optional[Path]) -> Dict[str, dict]:
    """experiment name -> {"tests": {test name: {fire_at, duration, test}}, "failures": [s], "routers": {names}}."""
    index: Dict[str, dict] = {}
    if experiments_dir is None or not experiments_dir.is_dir():
        return index
    for path in sorted(experiments_dir.rglob("*.json")):
        try:
            experiment = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        name = experiment.get("experiment_name")
        if not name:
            continue
        network = experiment.get("network", {})
        nodes = network.get("nodes", {})
        routers = {normalize_node(n) for n, node in nodes.items() if isinstance(node, dict) and "Router" in node}
        failures = [float(e["happens_at"]) for e in network.get("failure_events", []) if "happens_at" in e]
        tests = {
            t["name"]: {"fire_at": float(t.get("fire_at", 0)), "duration": float(t.get("duration", 0)), "test": t.get("test", "")}
            for t in experiment.get("test_batch", [])
            if "name" in t
        }
        index[name] = {"tests": tests, "failures": sorted(failures), "routers": routers or None}
    return index


def load_os_metadata(os_file: Optional[Path], families_file: Optional[Path]) -> Dict[str, Dict[str, str]]:
    """os -> {network_stack, routing_stack, os_family}."""
    metadata: Dict[str, Dict[str, str]] = {}
    if os_file is not None and os_file.is_file():
        with open(os_file, "rb") as file:
            for name, os_config in tomllib.load(file).items():
                if isinstance(os_config, dict):
                    metadata[name] = {
                        "network_stack": str(os_config.get("network_stack") or "none"),
                        "routing_stack": str(os_config.get("routing_stack") or "none"),
                    }
    if families_file is not None and families_file.is_file():
        with open(families_file, "rb") as file:
            for name, family in tomllib.load(file).get("families", {}).items():
                metadata.setdefault(name, {})["os_family"] = str(family)
    return metadata


def parse_experiment_name(name: str) -> Optional[Dict[str, str]]:
    parts = name.split(",")
    if len(parts) != len(config.NAME_FIELDS):
        return None
    return dict(zip(config.NAME_FIELDS, parts))


def matches(factors: Dict[str, str], filters: Dict[str, Optional[str]]) -> bool:
    for key, wanted in filters.items():
        if wanted is None:
            continue
        if key == "failures":
            wanted = "failures" if str(wanted).lower() in ("true", "1", "yes", "failures") else "no-failures"
        if factors.get(key) != wanted:
            return False
    return True


def run_dirs(experiment_dir: Path) -> List[Path]:
    def key(path: Path):
        digits = re.findall(r"\d+", path.name)
        return int(digits[0]) if digits else 0

    return sorted((p for p in experiment_dir.iterdir() if p.is_dir() and p.name.startswith(RUN_PREFIX)), key=key)


# --------------------------------------------------------------------------- extraction


def extract_experiment(args: Tuple[Path, Dict[str, str], dict, Dict[str, str]]) -> Tuple[List[dict], List[dict]]:
    """Rows of ``runs.csv`` and ``routers.csv`` for one experiment folder."""
    experiment_dir, factors, info, os_info = args
    name = experiment_dir.name
    tests_info: Dict[str, dict] = info.get("tests", {})
    failures: List[float] = info.get("failures", [])
    routers = info.get("routers")
    multi_test = len(tests_info) > 1
    vcpus = config.VCPUS.get(factors["resources"])

    run_rows: List[dict] = []
    router_rows: List[dict] = []
    for run_dir in run_dirs(experiment_dir):
        run = run_dir.name
        test_dirs = [d for d in sorted(run_dir.iterdir()) if d.is_dir() and d.name != "host_resources"]
        for test_dir in test_dirs:
            flent_files = sorted(test_dir.glob("*.flent.gz")) or sorted(test_dir.glob("*.flent"))
            if not flent_files:
                continue
            test_info = tests_info.get(test_dir.name, {})
            fire_at = float(test_info.get("fire_at", 0.0))
            # Failures are timed from the experiment start, flent from its own start
            offsets = [f - fire_at for f in failures if f > fire_at] if factors["failures"] == "failures" else []
            try:
                data = load_flent(flent_files[0])
                metrics = run_metrics(data, offsets)
            except (OSError, ValueError, KeyError) as error:
                LOG.warning("%s/%s/%s: cannot read %s (%s)", name, run, test_dir.name, flent_files[0].name, error)
                continue

            row = {
                "experiment": name,
                "test": test_dir.name,
                "run": run,
                **factors,
                "test_batch": f"{factors['test_batch']} / {test_dir.name}" if multi_test else factors["test_batch"],
                "vcpus": vcpus,
                **{k: os_info.get(k, "unknown") for k in config.ARCHITECTURE_FACTORS},
                **describe(data),
                **metrics,
            }

            load = (fire_at + metrics.get("load_start_s", 0.0), fire_at + metrics.get("load_end_s", 0.0))
            node_rows, aggregates = run_resources(run_dir, load, routers, vcpus)
            row.update(aggregates)
            total = row.get("tcp_total_mbps")
            if total is not None:
                if row.get("routers_guest_cores", 0) > 0:
                    row["mbps_per_guest_core"] = total / row["routers_guest_cores"]
                if row.get("routers_host_cores", 0) > 0:
                    row["mbps_per_host_core"] = total / row["routers_host_cores"]
            run_rows.append(row)
            for node_row in node_rows:
                router_rows.append({"experiment": name, "test": test_dir.name, "run": run, **factors, **node_row})
    return run_rows, router_rows


def build_runs(
    results_dir: Path,
    experiments_dir: Optional[Path] = None,
    os_file: Optional[Path] = None,
    families_file: Optional[Path] = None,
    filters: Optional[Dict[str, Optional[str]]] = None,
    jobs: int = 1,
) -> Tuple[pd.DataFrame, pd.DataFrame]:
    index = load_experiment_index(experiments_dir)
    os_metadata = load_os_metadata(os_file, families_file)
    filters = filters or {}

    tasks = []
    for experiment_dir in sorted(p for p in results_dir.iterdir() if p.is_dir()):
        factors = parse_experiment_name(experiment_dir.name)
        if factors is None:
            LOG.debug("Skipping %s: not an experiment folder", experiment_dir.name)
            continue
        if not matches(factors, filters):
            continue
        if experiment_dir.name not in index and index:
            LOG.warning("%s: no experiment file, fire times assumed 0 and failures unknown", experiment_dir.name)
        tasks.append((experiment_dir, factors, index.get(experiment_dir.name, {}), os_metadata.get(factors["os"], {})))

    LOG.info("Extracting %d experiment(s) with %d job(s)", len(tasks), jobs)
    run_rows: List[dict] = []
    router_rows: List[dict] = []
    if jobs > 1 and len(tasks) > 1:
        with ProcessPoolExecutor(max_workers=jobs) as pool:
            results = list(pool.map(extract_experiment, tasks, chunksize=4))
    else:
        results = [extract_experiment(task) for task in tasks]
    for runs, routers in results:
        run_rows.extend(runs)
        router_rows.extend(routers)

    runs_df = pd.DataFrame(run_rows)
    routers_df = pd.DataFrame(router_rows)
    if not runs_df.empty:
        runs_df["unit"] = runs_df["experiment"] + " | " + runs_df["test"]
        runs_df["scenario"] = scenario_key(runs_df)
    return runs_df, routers_df


def scenario_key(df: pd.DataFrame) -> pd.Series:
    """Everything but the OS: the conditions in which OSes are compared."""
    columns = [f for f in config.FACTORS if f != "os" and f in df.columns]
    return df[columns].astype(str).agg(",".join, axis=1)


# --------------------------------------------------------------------------- aggregation


def bootstrap_median_ci(values: np.ndarray, rng: np.random.Generator, n_boot: int, alpha: float = 0.05) -> Tuple[float, float]:
    values = values[~np.isnan(values)]
    if len(values) == 0:
        return math.nan, math.nan
    if len(values) == 1:
        return float(values[0]), float(values[0])
    samples = rng.choice(values, size=(n_boot, len(values)), replace=True)
    medians = np.median(samples, axis=1)
    return float(np.quantile(medians, alpha / 2)), float(np.quantile(medians, 1 - alpha / 2))


def metric_columns(df: pd.DataFrame) -> List[str]:
    known = [m.name for m in config.METRICS if m.name in df.columns]
    return known


def aggregate_experiments(runs: pd.DataFrame, n_boot: int = 2000, seed: int = 0) -> pd.DataFrame:
    """One row per unit (experiment x test): median, bootstrap CI and CV of every metric."""
    if runs.empty:
        return pd.DataFrame()
    rng = np.random.default_rng(seed)
    descriptors = ["unit", "experiment", "test", "scenario", *config.FACTORS, *config.ARCHITECTURE_FACTORS, "vcpus"]
    descriptors = [c for c in descriptors if c in runs.columns]
    metrics = metric_columns(runs)
    extra = [c for c in ("endpoint_saturated", "cpu_from_guest", "recovered") if c in runs.columns]

    rows = []
    for unit, group in runs.groupby("unit", sort=True):
        row = {c: group[c].iloc[0] for c in descriptors}
        row["n_runs"] = int(group["run"].nunique())
        for column in extra:
            row[column] = float(group[column].max()) if group[column].notna().any() else math.nan
        for name in metrics:
            values = pd.to_numeric(group[name], errors="coerce").to_numpy(dtype=float)
            present = values[~np.isnan(values)]
            if len(present) == 0:
                continue
            row[name] = float(np.median(present))
            row[f"{name}_mean"] = float(np.mean(present))
            row[f"{name}_n"] = int(len(present))
            if len(present) > 1:
                sd = float(np.std(present, ddof=1))
                row[f"{name}_sd"] = sd
                mean = float(np.mean(present))
                row[f"{name}_cv_pct"] = sd / abs(mean) * 100.0 if abs(mean) > 1e-12 else math.nan
            low, high = bootstrap_median_ci(present, rng, n_boot)
            row[f"{name}_ci_lo"], row[f"{name}_ci_hi"] = low, high
        rows.append(row)
    return pd.DataFrame(rows)


def write_tables(output_dir: Path, runs: pd.DataFrame, routers: pd.DataFrame, experiments: pd.DataFrame) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    runs.to_csv(output_dir / "runs.csv", index=False)
    routers.to_csv(output_dir / "routers.csv", index=False)
    experiments.to_csv(output_dir / "experiments.csv", index=False)


def default_jobs() -> int:
    return max(1, min(8, (os.cpu_count() or 2) - 1))
