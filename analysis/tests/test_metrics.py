import math
from datetime import datetime, timezone

import numpy as np
import pytest

from analysis import flent_metrics, resource_metrics
from analysis.tests import synthetic


def make_flent(tmp_path, total=8000.0, outage=None):
    path = tmp_path / "RRUL" / "rrul.flent.gz"
    t0 = datetime(2026, 1, 1, tzinfo=timezone.utc)
    synthetic.flent_file(path, t0, total, np.random.default_rng(0), outage)
    return flent_metrics.load_flent(path)


def test_throughput_latency_and_fairness(tmp_path):
    metrics = flent_metrics.run_metrics(make_flent(tmp_path))
    assert metrics["load_start_s"] == pytest.approx(5.2)
    assert metrics["tcp_total_mbps"] == pytest.approx(8000, rel=0.02)
    assert metrics["tcp_download_mbps"] == pytest.approx(0.55 * 8000, rel=0.02)
    # 4 flows per direction, shares 28/24/24/24 %: close to but below 1
    assert 0.95 < metrics["jain_fairness"] < 1.0
    assert metrics["icmp_idle_median_ms"] == pytest.approx(0.5, rel=0.1)
    assert metrics["latency_induced_ms"] > 0
    assert metrics["ef_be_latency_ratio"] == pytest.approx(0.8, rel=0.05)


@pytest.mark.parametrize("outage", [2.0, 5.0, 10.0])
def test_failure_outage_and_recovery(tmp_path, outage):
    data = make_flent(tmp_path, outage=outage)
    metrics = flent_metrics.run_metrics(data, [synthetic.FAILURE_AT])
    assert metrics["outage_s"] == pytest.approx(outage, abs=0.25)
    assert metrics["recovery_s"] == pytest.approx(outage, abs=0.25)
    assert metrics["recovered"] == 1.0
    assert metrics["failure_throughput_loss_pct"] == pytest.approx(outage / 10 * 100, abs=5)


def test_no_failure_metrics_without_failure(tmp_path):
    assert "outage_s" not in flent_metrics.run_metrics(make_flent(tmp_path))


def test_jain_index():
    assert flent_metrics.jain_index([1, 1, 1, 1]) == pytest.approx(1.0)
    assert flent_metrics.jain_index([1, 0, 0, 0]) == pytest.approx(0.25)
    assert math.isnan(flent_metrics.jain_index([]))


def test_parse_time_any_precision():
    a = flent_metrics.parse_time("2026-10-30T23:01:55.742040Z")
    b = flent_metrics.parse_time("2026-10-30T23:01:55.74204Z")
    assert a == pytest.approx(b)


def test_cores_and_idle_router(synthetic_tree):
    run_dir = next(synthetic_tree["results"].glob("Double,Fast,*High Resources,VirtIO,static,no-failures/run_1"))
    rows, aggregates = resource_metrics.run_resources(run_dir, (5.2, 65.0), None, 8)
    by_node = {r["node"]: r for r in rows}
    assert by_node["Router_3"]["active"] is False
    assert by_node["Router_1"]["active"] is True
    assert by_node["Client"]["role"] == "endpoint"
    # 9000 Mbit/s x 0.10 core per Gbit/s = 0.9 core per router, whatever the vCPU count
    assert by_node["Router_1"]["guest_cores"] == pytest.approx(0.9, rel=0.1)
    assert aggregates["active_routers"] == 2
    assert aggregates["max_router_mem_delta_mib"] == pytest.approx(5.0)
    assert aggregates["emulation_overhead"] == pytest.approx(1.3, rel=0.05)
    assert aggregates["endpoint_saturated"] == 0.0


def test_node_names_are_normalized():
    assert resource_metrics.normalize_node("AS1 Router 1") == resource_metrics.normalize_node("AS1_Router_1")
