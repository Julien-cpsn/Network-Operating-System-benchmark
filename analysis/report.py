"""Step 7 - ``report.md``: the main numbers of every step, with links to the tables and figures."""
from __future__ import annotations

import math
from pathlib import Path
from typing import List

import pandas as pd

from analysis import config


def _fmt(value, digits: int = 2) -> str:
    if value is None or (isinstance(value, float) and (math.isnan(value) or math.isinf(value))):
        return "-" if not (isinstance(value, float) and math.isinf(value)) else "∞"
    if isinstance(value, float):
        if abs(value) >= 1000:
            return f"{value:,.0f}"
        return f"{value:.{digits}f}"
    return str(value)


def table(df: pd.DataFrame, columns: List[str], digits: int = 2, max_rows: int = 20) -> str:
    if df is None or df.empty:
        return "_none_\n"
    columns = [c for c in columns if c in df.columns]
    lines = ["| " + " | ".join(columns) + " |", "|" + "---|" * len(columns)]
    for _, row in df.head(max_rows).iterrows():
        lines.append("| " + " | ".join(_fmt(row[c], digits) for c in columns) + " |")
    if len(df) > max_rows:
        lines.append(f"\n_{len(df) - max_rows} more rows in the CSV file._")
    return "\n".join(lines) + "\n"


def write(
    path: Path,
    runs: pd.DataFrame,
    experiments: pd.DataFrame,
    precision_summary: pd.DataFrame,
    reruns: pd.DataFrame,
    validity: pd.DataFrame,
    blocks: pd.DataFrame,
    effects: pd.DataFrame,
    importance: pd.DataFrame,
    interactions: pd.DataFrame,
    ranking: pd.DataFrame,
    pareto_share: pd.DataFrame,
    sensitivity: pd.DataFrame,
    correlations: pd.DataFrame,
    figures: List[Path],
) -> Path:
    out: List[str] = ["# Cross-experiment analysis\n"]
    out.append(
        f"{experiments['experiment'].nunique() if not experiments.empty else 0} experiments, "
        f"{len(experiments)} experiment x test units, {len(runs)} runs; "
        f"{experiments['os'].nunique() if not experiments.empty else 0} OSes, "
        f"{experiments['scenario'].nunique() if not experiments.empty else 0} scenarios (every factor but the OS).\n"
    )
    if not experiments.empty:
        levels = {f: ", ".join(config.order_levels(f, experiments[f].unique())) for f in config.FACTORS if f in experiments.columns}
        out.append("\n".join(f"- **{f}**: {v}" for f, v in levels.items()) + "\n")

    out.append("\n## 1. Measurement noise\n")
    out.append("Run-to-run variability per metric (CI = 95 % confidence interval of the mean, target ±5 %). "
               "Differences smaller than the CI cannot be told apart from noise. Details: `noise/precision.csv`.\n\n")
    out.append(table(precision_summary, ["metric", "units", "median_cv_pct", "p90_cv_pct", "share_within_5pct", "median_runs_needed"]))
    if reruns is not None and not reruns.empty:
        counts = reruns.groupby("metric").size().rename("ambiguous OS pairs").reset_index()
        out.append(f"\n{len(reruns)} pairs of OSes adjacent in a scenario's ranking have overlapping CIs: their order is not "
                   "established. Re-running only these is enough (`noise/rerun_candidates.csv`, with the runs needed to separate them).\n\n")
        out.append(table(counts, ["metric", "ambiguous OS pairs"]))
    if validity is not None and not validity.empty:
        flags = validity.drop(columns=["unit", "n_runs"]).sum().rename("units").reset_index().rename(columns={"index": "flag"})
        out.append("\nUnits to read with care (`noise/validity.csv`): a saturated client/server limits the throughput "
                   "instead of the routers; guest-side CPU is not comparable between OSes.\n\n")
        out.append(table(flags, ["flag", "units"]))

    out.append("\n## 2. Design coverage\n")
    out.append("Blocks modelled separately (one per topology). Aliased factors cannot be separated from the ones they "
               "go with; incomplete interactions lack some cells. Counts per pair of levels: `design/coverage.csv`.\n\n")
    out.append(table(blocks, list(blocks.columns) if blocks is not None and not blocks.empty else [], max_rows=50))

    out.append("\n## 3. Which factors matter (effect sizes)\n")
    out.append("Partial ω² of each term of `y ~ os + factors + os:factor` (type II ANOVA, log scale for multiplicative "
               "metrics). Rule of thumb: 0.01 small, 0.06 medium, 0.14 large. All terms: `effects/effects.csv`.\n")
    if effects is not None and not effects.empty:
        os_effects = effects[effects["model"] == "os"]
        for response in [r for r in config.KEY_METRICS if r in set(os_effects["response"])]:
            part = os_effects[os_effects["response"] == response]
            best = part.sort_values("partial_omega2", ascending=False).groupby("block").head(4)
            out.append(f"\n**{config.metric(response).label}**\n\n")
            out.append(table(best.sort_values(["block", "partial_omega2"], ascending=[True, False]), ["block", "term", "partial_omega2", "share_ss", "p_value", "r2"], digits=3))
        arch = effects[(effects["model"] == "architecture") & (effects["term"].isin(config.ARCHITECTURE_FACTORS))]
        if not arch.empty:
            out.append("\n**Architecture (OS replaced by family, network stack, routing stack)**\n\n")
            out.append(table(arch.sort_values("partial_omega2", ascending=False), ["block", "response", "term", "partial_omega2", "p_value"], digits=3))

    out.append("\n## 4. Model-based cross-check\n")
    out.append("Gradient boosting: R² lost when a factor is shuffled (out-of-fold, folds grouped by experiment). "
               "A low CV R² means the factors do not explain the metric well: do not over-read its ranking.\n\n")
    if importance is not None and not importance.empty:
        top = importance.sort_values(["response", "importance"], ascending=[True, False]).groupby("response").head(3)
        out.append(table(top, ["response", "factor", "importance", "cv_r2"], digits=3, max_rows=60))
    if interactions is not None and not interactions.empty:
        out.append("\nStrongest interactions (Friedman H², share of the joint effect that is not additive):\n\n")
        strong = interactions[(interactions["h2"] > 0.05) & (interactions["joint_effect_share"] >= 0.02)]
        strong = strong.sort_values("h2", ascending=False)
        out.append(table(strong, ["response", "factor_a", "factor_b", "h2", "joint_effect_share"], digits=3))

    out.append("\n## 5. Which OS is best (Friedman + Nemenyi)\n")
    out.append("Average rank over the scenarios (1 = best). Two OSes whose ranks differ by less than the critical "
               "difference (CD) are not significantly different. Per topology: `ranking/ranking.csv`.\n")
    if ranking is not None and not ranking.empty:
        for name, part in ranking[ranking["scope"] == "all"].groupby("metric", sort=False):
            first = part.iloc[0]
            out.append(f"\n**{config.metric(name).label}** - k = {first['k']}, N = {first['n_scenarios']}, "
                       f"Friedman p = {_fmt(first['p_value'], 4)}, CD = {_fmt(first['critical_difference'])}"
                       + (f", excluded (low coverage): {first['excluded_oses']}" if first["excluded_oses"] else "") + "\n\n")
            out.append(table(part.sort_values("average_rank"), ["os", "average_rank", "wins"]))

    out.append("\n## 6. Trade-offs\n")
    out.append(f"Share of scenarios where the OS is on the Pareto front of ({', '.join(config.PARETO_OBJECTIVES)}):\n\n")
    out.append(table(pareto_share, ["os", "scenarios", "on_front", "share_on_front"]))
    if sensitivity is not None and not sensitivity.empty:
        out.append("\nSensitivity (median over the scenarios; ratio for multiplicative metrics, difference otherwise):\n\n")
        main = sensitivity[sensitivity["metric"].isin(["tcp_total_mbps", "icmp_latency_p95_ms", "mbps_per_guest_core"])]
        out.append(table(main.sort_values(["factor", "metric", "median_change"]), ["factor", "from", "to", "metric", "os", "median_change", "q1", "q3", "contexts"], max_rows=80))
    if correlations is not None and not correlations.empty:
        strong = correlations[correlations["spearman_rho"].abs() >= 0.7].sort_values("spearman_rho", key=lambda s: -s.abs())
        out.append("\nStrongly correlated metrics (|ρ| ≥ 0.7, partly redundant):\n\n")
        out.append(table(strong, ["metric_a", "metric_b", "spearman_rho", "n"]))

    out.append("\n## Figures\n\n")
    root = path.parent
    for figure in figures:
        try:
            out.append(f"- [{figure.relative_to(root)}]({figure.relative_to(root)})\n")
        except ValueError:
            out.append(f"- {figure}\n")

    path.write_text("".join(out), encoding="utf-8")
    return path
