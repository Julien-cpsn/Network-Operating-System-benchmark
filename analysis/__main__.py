"""``python -m analysis``: extract every result into tidy tables, then run every analysis.

Usually called through ``cargo run -- analyze`` (same experiment selection arguments).
"""
from __future__ import annotations

import argparse
import logging
import sys
from pathlib import Path
from typing import List

import pandas as pd

from analysis import config, dataset, design, effects, importance, noise, plotting, ranking, report, tradeoffs

LOG = logging.getLogger("analysis")


def parse_args(argv: List[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="python -m analysis", description=__doc__)
    root = Path.cwd() / "experimentation"
    parser.add_argument("--results-dir", type=Path, default=root / "results")
    parser.add_argument("--experiments-dir", type=Path, default=root / "experiments")
    parser.add_argument("--os-file", type=Path, default=root / "operating_systems.toml")
    parser.add_argument("--families-file", type=Path, default=root / "os_families.toml")
    parser.add_argument("--output-dir", type=Path, default=root / "analysis")

    selection = parser.add_argument_group("experiment selection (all if absent)")
    selection.add_argument("--os")
    selection.add_argument("--topology")
    selection.add_argument("--test-batch")
    selection.add_argument("--resources", help='e.g. "High Resources"')
    selection.add_argument("--nic", help="RTL8139, E1000 or VirtIO")
    selection.add_argument("--protocol", help="static, RIP, OSPF, BGP, MPLS, mixed")
    selection.add_argument("--failures", help="true or false")

    parser.add_argument("--reuse", action="store_true", help="reuse data/runs.csv instead of re-reading the results")
    parser.add_argument("--jobs", type=int, default=dataset.default_jobs(), help="parallel extraction processes")
    parser.add_argument("--bootstrap", type=int, default=2000, help="bootstrap resamples for the confidence intervals")
    parser.add_argument("--metrics", help="comma-separated metrics for the effect/model/ranking steps (default: key metrics)")
    parser.add_argument("--min-coverage", type=float, default=0.8, help="share of the scenarios an OS needs to be ranked")
    parser.add_argument("--no-model", action="store_true", help="skip the gradient boosting step (the slowest)")
    parser.add_argument("--no-plots", action="store_true")
    parser.add_argument("-v", "--verbose", action="count", default=0)
    return parser.parse_args(argv)


def plot_coverage(experiments: pd.DataFrame, output_dir: Path) -> Path:
    columns = [f for f in ("topology", "test_batch", "routing", "failures") if f in experiments.columns and experiments[f].nunique() >= 1]
    key = experiments[columns].astype(str).agg(" | ".join, axis=1)
    table = pd.crosstab(experiments["os"], key)
    fig, ax = plotting.plt.subplots(figsize=(2.5 + 0.45 * table.shape[1], 1.4 + 0.35 * table.shape[0]))
    image = plotting.heatmap(ax, table.to_numpy(dtype=float), list(table.index), list(table.columns), vmin=0, fmt="{:.0f}")
    fig.colorbar(image, ax=ax, label="experiment x test units (resources x NIC)")
    ax.set_title(f"Coverage ({' | '.join(columns)})")
    return plotting.save(fig, output_dir / "coverage.png")


def main(argv: List[str] = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    logging.basicConfig(level=logging.DEBUG if args.verbose > 1 else logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    filters = {"os": args.os, "topology": args.topology, "test_batch": args.test_batch, "resources": args.resources, "nic": args.nic, "routing": args.protocol, "failures": args.failures}
    selected = {k: v for k, v in filters.items() if v is not None}
    # A selection gets its own folder, so that it never overwrites the analysis of everything
    out = args.output_dir / plotting.slug("_".join(f"{k}-{v}" for k, v in selected.items())) if selected else args.output_dir
    data_dir = out / "data"
    full_runs = args.output_dir / "data" / "runs.csv"

    # 1. Tidy tables
    if args.reuse and full_runs.is_file():
        runs = pd.read_csv(full_runs, low_memory=False)
        routers_file = args.output_dir / "data" / "routers.csv"
        routers = pd.read_csv(routers_file, low_memory=False) if routers_file.is_file() else pd.DataFrame()
        keep = [dataset.matches({f: str(r[f]) for f in config.NAME_FIELDS}, filters) for _, r in runs.iterrows()]
        runs = runs[keep]
        if not routers.empty:
            routers = routers[routers["experiment"].isin(set(runs["experiment"]))]
        LOG.info("Reusing %d runs from %s", len(runs), full_runs)
    else:
        if not args.results_dir.is_dir():
            LOG.error("No results folder %s (run experiments first)", args.results_dir)
            return 1
        runs, routers = dataset.build_runs(args.results_dir, args.experiments_dir, args.os_file, args.families_file, filters, args.jobs)
    if runs.empty:
        LOG.error("No run found in %s for this selection", args.results_dir)
        return 1
    for column in config.FACTORS + config.ARCHITECTURE_FACTORS:
        if column in runs.columns:
            runs[column] = runs[column].astype(str)
    experiments = dataset.aggregate_experiments(runs, n_boot=args.bootstrap)
    dataset.write_tables(data_dir, runs, routers, experiments)
    LOG.info("%d runs, %d units, %d scenarios -> %s", len(runs), len(experiments), experiments["scenario"].nunique(), data_dir)

    metrics = [m for m in (args.metrics.split(",") if args.metrics else config.KEY_METRICS) if m in runs.columns]
    all_metrics = dataset.metric_columns(runs)
    style = plotting.OsStyle(runs["os"].unique())
    figures: List[Path] = []
    plots = not args.no_plots

    # 2. Noise
    noise_dir = out / "noise"
    noise_dir.mkdir(parents=True, exist_ok=True)
    precision = noise.precision_table(experiments, all_metrics)
    precision_summary = noise.precision_summary(precision)
    reruns = noise.rerun_candidates(experiments, metrics)
    validity = noise.validity_table(experiments)
    precision.to_csv(noise_dir / "precision.csv", index=False)
    precision_summary.to_csv(noise_dir / "precision_summary.csv", index=False)
    reruns.to_csv(noise_dir / "rerun_candidates.csv", index=False)
    validity.to_csv(noise_dir / "validity.csv", index=False)

    # 3. Design
    design_dir = out / "design"
    design_dir.mkdir(parents=True, exist_ok=True)
    design.coverage(experiments).to_csv(design_dir / "coverage.csv", index=False)
    blocks = pd.DataFrame([b.as_row() for b in design.blocks(experiments)])
    blocks.to_csv(design_dir / "blocks.csv", index=False)
    if plots:
        figures.append(plot_coverage(experiments, design_dir))

    # 4. Effect sizes
    effects_dir = out / "effects"
    effects_dir.mkdir(parents=True, exist_ok=True)
    effect_table = effects.effect_sizes(runs, experiments, metrics)
    effect_table.to_csv(effects_dir / "effects.csv", index=False)
    if plots:
        figures += effects.plot_effects(effect_table, effects_dir)
        figures += effects.plot_interactions(experiments, effects_dir, [m for m in ("tcp_total_mbps", "icmp_latency_p95_ms", "mbps_per_guest_core") if m in metrics], style)
    LOG.info("Effect sizes: %d terms", len(effect_table))

    # 4b. Model-based importance
    importance_table = interactions = pd.DataFrame()
    if not args.no_model:
        importance_dir = out / "importance"
        importance_dir.mkdir(parents=True, exist_ok=True)
        importance_table, interactions = importance.analyse(runs, metrics)
        importance_table.to_csv(importance_dir / "importance.csv", index=False)
        interactions.to_csv(importance_dir / "interactions_h.csv", index=False)
        if plots:
            figures += importance.plot(importance_table, interactions, importance_dir)

    # 5. Ranking
    ranking_dir = out / "ranking"
    ranking_dir.mkdir(parents=True, exist_ok=True)
    ranking_table, pairs, results = ranking.rank_all(experiments, metrics, args.min_coverage)
    ranking_table.to_csv(ranking_dir / "ranking.csv", index=False)
    pairs.to_csv(ranking_dir / "ranking_pairs.csv", index=False)
    if plots:
        figures += ranking.plot_rankings(results, ranking_dir)
        for name in ("tcp_total_mbps", "icmp_latency_p95_ms", "mbps_per_guest_core"):
            path = ranking.plot_scoreboard(experiments, name, ranking_dir)
            if path:
                figures.append(path)

    # 6. Trade-offs
    tradeoffs_dir = out / "tradeoffs"
    tradeoffs_dir.mkdir(parents=True, exist_ok=True)
    front = tradeoffs.pareto(experiments)
    share = tradeoffs.pareto_share(front)
    correlation_table = tradeoffs.correlations(experiments)
    sensitivity_table = tradeoffs.sensitivity(experiments, all_metrics)
    front.to_csv(tradeoffs_dir / "pareto.csv", index=False)
    share.to_csv(tradeoffs_dir / "pareto_share.csv", index=False)
    correlation_table.to_csv(tradeoffs_dir / "correlations.csv", index=False)
    sensitivity_table.to_csv(tradeoffs_dir / "sensitivity.csv", index=False)
    if plots:
        for path in (tradeoffs.plot_pareto(front, tradeoffs_dir, style), tradeoffs.plot_correlations(correlation_table, tradeoffs_dir)):
            if path:
                figures.append(path)
        figures += tradeoffs.plot_sensitivity(sensitivity_table, tradeoffs_dir)

    # 7. Report
    path = report.write(
        out / "report.md", runs, experiments, precision_summary, reruns, validity, blocks, effect_table,
        importance_table, interactions, ranking_table, share, sensitivity_table, correlation_table, figures,
    )
    LOG.info("Report: %s", path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
