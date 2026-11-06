"""Synthetic results tree with known effects, laid out exactly like ``run`` writes it.

Ground truth:
- OS "Fast" > "Scaling" > "Slow" on throughput at 2 vCPUs;
- "Scaling" doubles its throughput with each resource level (OS x resources interaction),
  the others do not scale;
- VirtIO > E1000 > RTL8139 for every OS (strong NIC main effect);
- the routing protocol has no effect;
- on "Double" a third router is never used (idle, must not count as active);
- after the link failure, the outage lasts 2 s (Fast), 5 s (Scaling), 10 s (Slow).
"""
from __future__ import annotations

import gzip
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np

OSES = {"Fast": 9000.0, "Scaling": 4000.0, "Slow": 2500.0}
OS_TOML = """
[Fast]
    input_ready = "#"
    network_stack = "Linux network stack"
    routing_stack = "FRR"
[Scaling]
    input_ready = "#"
    network_stack = "FreeBSD network stack"
    routing_stack = "FRR"
[Slow]
    input_ready = "#"
    network_stack = "FreeBSD network stack"
    routing_stack = "BIRD2"
"""
NIC_FACTOR = {"VirtIO": 1.0, "E1000": 0.4, "RTL8139": 0.15}
RESOURCES = {"Low Resources": 2, "Medium Resources": 4, "High Resources": 8}
OUTAGE = {"Fast": 2.0, "Scaling": 5.0, "Slow": 10.0}
CORES_PER_GBPS = {"Fast": 0.10, "Scaling": 0.25, "Slow": 0.40}
TOPOLOGIES = {
    "Simple": {"routers": ["Router"], "idle": []},
    "Double": {"routers": ["Router 1", "Router 2"], "idle": ["Router 3"]},
}
STEP = 0.2
LENGTH = 70.0
LOAD = (5.2, 65.0)
FAILURE_AT = 30.0


def _iso(t: datetime) -> str:
    return t.strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def throughput(os_name: str, resources: str, nic: str) -> float:
    base = OSES[os_name] * NIC_FACTOR[nic]
    if os_name == "Scaling":
        base *= {"Low Resources": 1.0, "Medium Resources": 2.0, "High Resources": 4.0}[resources]
    return base


def flent_file(path: Path, t0: datetime, total: float, rng: np.random.Generator, outage: Optional[float]) -> None:
    x = np.round(np.arange(0.0, LENGTH, STEP), 3)
    active = (x >= LOAD[0]) & (x <= LOAD[1])
    totals = np.where(active, total * (1 + rng.normal(0, 0.03, len(x))), np.nan)
    if outage is not None:
        totals[(x >= FAILURE_AT) & (x < FAILURE_AT + outage)] = 0.0
    results: Dict[str, List[Optional[float]]] = {}
    shares = {"BE": 0.28, "BK": 0.24, "CS5": 0.24, "EF": 0.24}

    def to_list(values):
        return [None if np.isnan(v) else float(v) for v in values]

    results["TCP totals"] = to_list(totals)
    results["TCP download sum"] = to_list(totals * 0.55)
    results["TCP upload sum"] = to_list(totals * 0.45)
    for direction, part in (("download", 0.55), ("upload", 0.45)):
        for cls, share in shares.items():
            results[f"TCP {direction} {cls}"] = to_list(totals * part * share)
    latency_load = 1.0 + 4000.0 / max(total, 1.0)
    pings = np.where(active, latency_load, 0.5) * (1 + rng.normal(0, 0.05, len(x)))
    for name, factor in (("Ping (ms) ICMP", 1.0), ("Ping (ms) UDP BE", 1.0), ("Ping (ms) UDP EF", 0.8), ("Ping (ms) UDP BK", 1.1)):
        results[name] = to_list(pings * factor)
    epoch = t0.timestamp()
    raw = {name: [{"t": epoch + float(xi), "val": v} for xi, v in zip(x, values) if v is not None] for name, values in results.items() if name.startswith("Ping")}
    data = {
        "metadata": {"NAME": "rrul", "T0": _iso(t0), "TITLE": path.parent.name, "LENGTH": 60, "STEP_SIZE": STEP},
        "x_values": [float(v) for v in x],
        "results": results,
        "raw_values": raw,
        "version": 4,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(path, "wt", encoding="utf-8") as file:
        json.dump(data, file)


def host_csv(path: Path, start: datetime, vcpus: int, cores_load: float, cores_idle: float, tx_pps: float, rng: np.random.Generator, busiest_cap: float = 100.0) -> None:
    lines = ["timestamp,vcpu,guest_pct,exit_pct,vhost_pct,qemu_pct,total_pct,rss_anon_mib,vm_tx_pps,vm_rx_pps,drop_pps,ns_per_packet,busiest_thread_pct,busiest_thread,busy_vhost_threads"]
    for second in range(-10, int(LENGTH) + 10):
        t = start + timedelta(seconds=second)
        loaded = LOAD[0] + 1 <= second <= LOAD[1] - 1
        cores = (cores_load if loaded else cores_idle) * (1 + rng.normal(0, 0.03))
        guest_pct = max(cores, 0.0) / vcpus * 100
        total_pct = guest_pct * 1.3
        pps = tx_pps if loaded else 5.0
        ns = 1e9 * cores * 1.3 / pps if pps > 1000 else ""
        busiest = min(busiest_cap, cores * 100 / max(1, vcpus) * 2)
        lines.append(f"{_iso(t)},{vcpus},{guest_pct:.2f},1.0,1.0,1.0,{total_pct:.2f},600.0,{pps:.0f},{pps:.0f},0,{ns},{busiest:.1f},CPU_0/KVM,1")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n")


def guest_log(path: Path, start: datetime, mem_idle: float, cpu_pct: float) -> None:
    lines = []
    for second in range(-5, int(LENGTH) + 5):
        loaded = LOAD[0] + 1 <= second <= LOAD[1] - 1
        used = mem_idle + (5.0 if loaded else 0.0)
        lines.append(f"{_iso(start + timedelta(seconds=second))} TRACE rx: 4000\t{4000 - used:.1f}\t{used:.1f}\t{cpu_pct if loaded else 0.1:.2f}")
    path.write_text("\n".join(lines) + "\n")


def build(root: Path, runs: int = 3, oses=None, seed: int = 1) -> Dict[str, Path]:
    """Writes ``results/``, ``experiments/``, ``operating_systems.toml``"""
    rng = np.random.default_rng(seed)
    results_dir, experiments_dir = root / "results", root / "experiments"
    results_dir.mkdir(parents=True, exist_ok=True)
    experiments_dir.mkdir(parents=True, exist_ok=True)
    (root / "operating_systems.toml").write_text(OS_TOML)
    clock = datetime(2026, 10, 30, 12, 0, 0, tzinfo=timezone.utc)

    for topology, layout in TOPOLOGIES.items():
        for os_name in oses or OSES:
            for resources, vcpus in RESOURCES.items():
                for nic in NIC_FACTOR:
                    for routing in ("static", "OSPF"):
                        for failures in (("no-failures", "failures") if topology == "Double" else ("no-failures",)):
                            name = f"{topology},{os_name},RRUL at start,{resources},{nic},{routing},{failures}"
                            nodes = {"Client": {"vcpu": 2, "ram": 2048, "Guest": {}}, "Server": {"vcpu": 2, "ram": 2048, "Guest": {}}}
                            for router in layout["routers"] + layout["idle"]:
                                nodes[router] = {"vcpu": vcpus, "ram": 4096, "Router": {}}
                            experiment = {
                                "experiment_name": name,
                                "network": {"nodes": nodes, "physical_links": [], "failure_events": [{"type": "link", "link_index": 1, "happens_at": FAILURE_AT}] if failures == "failures" else []},
                                "test_batch": [{"name": "RRUL", "test": "rrul", "fire_at": 0, "duration": 60, "from": "Client", "to": "Server"}],
                            }
                            (experiments_dir / f"{abs(hash(name))}.json").write_text(json.dumps(experiment))
                            total = throughput(os_name, resources, nic)
                            for run in range(1, runs + 1):
                                clock += timedelta(minutes=3)
                                run_dir = results_dir / name / f"run_{run}"
                                run_dir.mkdir(parents=True, exist_ok=True)
                                (run_dir / "experiment.log").write_text(
                                    f"{_iso(clock)}  INFO experiment: Experiment start\n{_iso(clock + timedelta(seconds=LENGTH))}  INFO experiment: Experiment end\n"
                                )
                                noisy_total = total * (1 + rng.normal(0, 0.04))
                                outage = OUTAGE[os_name] if failures == "failures" else None
                                flent_file(run_dir / "RRUL" / f"rrul-{run}.flent.gz", clock, noisy_total, rng, outage)
                                cores = noisy_total / 1000.0 * CORES_PER_GBPS[os_name]
                                mem_idle = {"Fast": 250.0, "Scaling": 300.0, "Slow": 400.0}[os_name]
                                for router in layout["routers"]:
                                    host_csv(run_dir / "host_resources" / f"{router}.csv", clock, vcpus, cores, 0.01, noisy_total * 80, rng)
                                    guest_log(run_dir / f"{router}.log", clock, mem_idle, cores / vcpus * 100)
                                for router in layout["idle"]:
                                    host_csv(run_dir / "host_resources" / f"{router}.csv", clock, vcpus, 0.01, 0.01, 2.0, rng)
                                    guest_log(run_dir / f"{router}.log", clock, mem_idle, 0.1)
                                for endpoint in ("Client", "Server"):
                                    host_csv(run_dir / "host_resources" / f"{endpoint}.csv", clock, 2, 1.0, 0.01, noisy_total * 80, rng, busiest_cap=60.0)
    return {
        "results": results_dir,
        "experiments": experiments_dir,
        "os_file": root / "operating_systems.toml",
    }
