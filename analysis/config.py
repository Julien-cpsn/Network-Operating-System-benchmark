"""Factors, metrics and their meaning, shared by every analysis step."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, List, Optional

# Experiment name = "{topology},{os},{test_batch},{resources},{nic},{routing},{failures}"
NAME_FIELDS: List[str] = ["topology", "os", "test_batch", "resources", "nic", "routing", "failures"]

# Factors varied by the benchmark (in the order they are listed in the reports)
FACTORS: List[str] = ["os", "topology", "test_batch", "resources", "nic", "routing", "failures"]

# Factors that describe the OS architecture (derived from `operating_systems.toml` and
# `os_families.toml`). Nested in `os`: never modelled together with it.
ARCHITECTURE_FACTORS: List[str] = ["os_family", "network_stack", "routing_stack"]

# Natural order of the ordinal factors (plots, sensitivity ratios)
LEVEL_ORDER: Dict[str, List[str]] = {
    "resources": ["Low Resources", "Medium Resources", "High Resources"],
    "nic": ["RTL8139", "E1000", "VirtIO"],
    "failures": ["no-failures", "failures"],
    "routing": ["static", "RIP", "OSPF", "BGP", "MPLS", "mixed"],
}

VCPUS: Dict[str, int] = {"Low Resources": 2, "Medium Resources": 4, "High Resources": 8}

# A router counts as "carrying traffic" above this many guest cores (or this packet rate)
ACTIVE_ROUTER_MIN_CORES = 0.05
ACTIVE_ROUTER_MIN_PPS = 1000.0

# Seconds trimmed at both ends of the load phase before computing steady-state resources
# (ramp-up, and the ~1 s offset between the host clock and the start of flent)
STEADY_STATE_TRIM_S = 3.0

# Failure metrics: throughput below OUTAGE_LEVEL x pre-failure level = outage,
# back above RECOVERY_LEVEL x pre-failure level = recovered
OUTAGE_LEVEL = 0.10
RECOVERY_LEVEL = 0.90
PRE_FAILURE_WINDOW_S = 10.0


@dataclass(frozen=True)
class Metric:
    name: str
    label: str
    higher_is_better: bool
    # Analysed on a log scale (positive, multiplicative effects: throughput, latency, resources)
    log: bool = True
    group: str = ""


METRICS: List[Metric] = [
    # Throughput
    Metric("tcp_download_mbps", "TCP download (Mbit/s)", True, group="throughput"),
    Metric("tcp_upload_mbps", "TCP upload (Mbit/s)", True, group="throughput"),
    Metric("tcp_total_mbps", "TCP total (Mbit/s)", True, group="throughput"),
    Metric("tcp_total_cv_pct", "Throughput instability within a run (CV %)", False, group="throughput"),
    # Latency
    Metric("icmp_latency_mean_ms", "ICMP latency under load, mean (ms)", False, group="latency"),
    Metric("icmp_latency_median_ms", "ICMP latency under load, median (ms)", False, group="latency"),
    Metric("icmp_latency_p95_ms", "ICMP latency under load, p95 (ms)", False, group="latency"),
    Metric("icmp_latency_p99_ms", "ICMP latency under load, p99 (ms)", False, group="latency"),
    Metric("icmp_idle_median_ms", "ICMP latency idle, median (ms)", False, group="latency"),
    Metric("latency_induced_ms", "Latency induced by the load (ms)", False, log=False, group="latency"),
    Metric("udp_ef_latency_ms", "UDP EF latency (ms)", False, group="latency"),
    Metric("udp_be_latency_ms", "UDP BE latency (ms)", False, group="latency"),
    # Fairness / DSCP handling
    Metric("jain_fairness", "Jain fairness of the TCP flows", True, log=False, group="fairness"),
    Metric("ef_be_latency_ratio", "EF / BE latency ratio (< 1: EF prioritised)", False, group="fairness"),
    # VoIP (voip, voip-rrul tests)
    Metric("voip_rtt_ms", "VoIP RTT (ms)", False, group="voip"),
    Metric("voip_induced_delay_ms", "VoIP induced delay (ms)", False, log=False, group="voip"),
    Metric("voip_jitter_ms", "VoIP jitter (ms)", False, group="voip"),
    Metric("voip_loss", "VoIP loss", False, log=False, group="voip"),
    # Link failures
    Metric("outage_s", "Outage after link failure (s)", False, log=False, group="failure"),
    Metric("recovery_s", "Recovery after link failure (s)", False, log=False, group="failure"),
    Metric("failure_throughput_loss_pct", "Throughput lost in the 10 s after the failure (%)", False, log=False, group="failure"),
    # Router resources (sum / max over the routers of the topology)
    Metric("routers_guest_cores", "Router CPU, guest code (cores, sum)", False, group="resources"),
    Metric("routers_host_cores", "Router CPU, incl. emulation/vhost (cores, sum)", False, group="resources"),
    Metric("max_router_guest_cores", "Busiest router CPU (cores)", False, group="resources"),
    Metric("max_busiest_thread_pct", "Busiest single thread (% of one core)", False, log=False, group="resources"),
    Metric("emulation_overhead", "Host CPU / guest CPU (NIC emulation cost)", False, group="resources"),
    Metric("ns_per_packet", "Host CPU time per packet (ns)", False, group="resources"),
    Metric("drop_pps", "Packets dropped by the host (pps, sum)", False, log=False, group="resources"),
    Metric("active_routers", "Routers carrying traffic", False, log=False, group="resources"),
    Metric("routers_mem_idle_mib", "Router memory idle (MiB, sum)", False, group="resources"),
    Metric("mean_router_mem_idle_mib", "Router memory idle (MiB, mean per router)", False, group="resources"),
    Metric("max_router_mem_delta_mib", "Router memory growth under load (MiB, max)", False, log=False, group="resources"),
    # Efficiency
    Metric("mbps_per_guest_core", "Throughput per router core (Mbit/s per core)", True, group="efficiency"),
    Metric("mbps_per_host_core", "Throughput per host core incl. emulation (Mbit/s per core)", True, group="efficiency"),
]

METRIC_BY_NAME: Dict[str, Metric] = {m.name: m for m in METRICS}

# Metrics analysed by default in the heavier steps (effects, model, ranking)
KEY_METRICS: List[str] = [
    "tcp_total_mbps",
    "tcp_download_mbps",
    "tcp_upload_mbps",
    "icmp_latency_p95_ms",
    "latency_induced_ms",
    "jain_fairness",
    "routers_guest_cores",
    "max_router_guest_cores",
    "routers_mem_idle_mib",
    "mbps_per_guest_core",
    "voip_induced_delay_ms",
    "outage_s",
]

# Objectives of the Pareto fronts: (metric, higher is better)
PARETO_OBJECTIVES: List[str] = ["tcp_total_mbps", "icmp_latency_p95_ms", "routers_guest_cores"]


def metric(name: str) -> Optional[Metric]:
    return METRIC_BY_NAME.get(name)


def order_levels(factor: str, levels) -> List[str]:
    """Levels of a factor in their natural order (known ones first, then alphabetical)."""
    levels = [str(level) for level in levels]
    known = LEVEL_ORDER.get(factor, [])
    return [level for level in known if level in levels] + sorted(level for level in levels if level not in known)
