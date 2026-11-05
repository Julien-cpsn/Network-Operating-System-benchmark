import pandas as pd
import pytest

from analysis import __main__ as cli
from analysis import dataset, design, importance, ranking


@pytest.fixture(scope="module")
def analysed(synthetic_tree, tmp_path_factory):
    out = tmp_path_factory.mktemp("analysis")
    code = cli.main([
        "--results-dir", str(synthetic_tree["results"]),
        "--experiments-dir", str(synthetic_tree["experiments"]),
        "--os-file", str(synthetic_tree["os_file"]),
        "--families-file", str(synthetic_tree["families_file"]),
        "--output-dir", str(out),
        "--jobs", "2",
        "--bootstrap", "200",
        "--no-model",
    ])
    assert code == 0
    return out


def test_tables(analysed):
    runs = pd.read_csv(analysed / "data" / "runs.csv")
    experiments = pd.read_csv(analysed / "data" / "experiments.csv")
    assert len(experiments) == 3 * 3 * 3 * 2 * (1 + 2)  # OS x resources x NIC x routing x (Simple + Double with/without failures)
    assert len(runs) == 3 * len(experiments)
    assert set(runs["network_stack"]) == {"Linux network stack", "FreeBSD network stack"}
    assert {"tcp_total_mbps_ci_lo", "tcp_total_mbps_ci_hi", "tcp_total_mbps_cv_pct"} <= set(experiments.columns)
    assert (experiments["tcp_total_mbps_ci_lo"] <= experiments["tcp_total_mbps"]).all()


def test_effect_sizes_recover_the_ground_truth(analysed):
    effects = pd.read_csv(analysed / "effects" / "effects.csv")
    total = effects[(effects["model"] == "os") & (effects["response"] == "tcp_total_mbps") & (effects["block"] == "Simple")].set_index("term")
    assert total.loc["os", "partial_omega2"] > 0.9
    assert total.loc["nic", "partial_omega2"] > 0.9
    assert total.loc["os:resources", "partial_omega2"] > 0.5
    assert total.loc["routing", "partial_omega2"] < 0.05
    assert total.loc["os:nic", "partial_omega2"] < 0.1
    arch = effects[(effects["model"] == "architecture") & (effects["response"] == "tcp_total_mbps")]
    assert {"network_stack", "routing_stack"} & set(arch["term"])


def test_ranking_and_pareto(analysed):
    table = pd.read_csv(analysed / "ranking" / "ranking.csv")
    total = table[(table["scope"] == "all") & (table["metric"] == "tcp_total_mbps")].set_index("os")
    assert total["average_rank"].idxmax() == "Slow"
    assert total["p_value"].iloc[0] < 0.001
    outage = table[(table["scope"] == "all") & (table["metric"] == "outage_s")].set_index("os")
    assert list(outage.sort_values("average_rank").index) == ["Fast", "Scaling", "Slow"]
    share = pd.read_csv(analysed / "tradeoffs" / "pareto_share.csv").set_index("os")
    assert share.loc["Slow", "share_on_front"] < share.loc["Fast", "share_on_front"]


def test_sensitivity(analysed):
    table = pd.read_csv(analysed / "tradeoffs" / "sensitivity.csv")
    scaling = table[(table["factor"] == "resources") & (table["metric"] == "tcp_total_mbps")].set_index("os")
    assert scaling.loc["Scaling", "median_change"] == pytest.approx(4.0, rel=0.1)
    assert scaling.loc["Fast", "median_change"] == pytest.approx(1.0, rel=0.1)
    nic = table[(table["factor"] == "nic") & (table["metric"] == "tcp_total_mbps")]
    assert (nic["median_change"] > 5).all()  # VirtIO / RTL8139 = 1 / 0.15


def test_report_and_figures(analysed):
    report = (analysed / "report.md").read_text()
    for section in ("Measurement noise", "Design coverage", "Which factors matter", "Which OS is best", "Trade-offs"):
        assert section in report
    assert list((analysed / "ranking").glob("cd_*.png"))
    assert (analysed / "tradeoffs" / "pareto.png").is_file()


def test_importance_ranks_os_and_nic_first(synthetic_tree):
    runs, _ = dataset.build_runs(synthetic_tree["results"], synthetic_tree["experiments"], jobs=2)
    table, h = importance.analyse(runs, ["tcp_total_mbps"])
    top = table.sort_values("importance", ascending=False)["factor"].head(2)
    assert set(top) == {"os", "nic"}
    assert table["cv_r2"].iloc[0] > 0.9
    pair = h[((h["factor_a"] == "os") & (h["factor_b"] == "resources"))]
    assert pair["h2"].iloc[0] > 0.05


def test_aliased_factor_is_dropped():
    df = pd.DataFrame({
        "topology": ["T"] * 8,
        "os": ["A", "A", "B", "B"] * 2,
        "nic": ["x", "y"] * 4,
        "routing": ["r1", "r1", "r2", "r2"] * 2,  # same as os: aliased
        "failures": ["no-failures"] * 8,
    })
    block = design.blocks(df)[0]
    assert "routing" in block.dropped
    assert block.factors == ["os", "nic"]


def test_critical_difference_known_value():
    # Demsar (2006), alpha = 0.05: q = 2.343 for k = 3
    assert ranking.critical_difference(3, 10) == pytest.approx(2.343 * (3 * 4 / 60) ** 0.5, rel=0.01)
