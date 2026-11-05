"""Step 5 - which OS is best across all the conditions? Friedman test + Nemenyi post-hoc.

Each scenario (every factor but the OS) is a "judge" that ranks the OSes on a metric
(rank 1 = best). The Friedman test asks whether the average ranks differ more than chance;
the Nemenyi critical difference (CD) tells which pairs differ:
``CD = q_alpha(k) * sqrt(k (k + 1) / (6 N))`` (k OSes, N scenarios). Ranks are robust to the
scenarios with extreme values and need no assumption about the distribution.

The test needs complete blocks: only scenarios where every compared OS has a value are used.
OSes covering less than ``min_coverage`` of the scenarios are left out (and listed), so that
one OS with few results does not shrink the comparison for all the others.
"""
from __future__ import annotations

import logging
import math
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np
import pandas as pd
from scipy import stats

from analysis import config, plotting

LOG = logging.getLogger("analysis.ranking")
ALPHA = 0.05


def critical_difference(k: int, n: int, alpha: float = ALPHA) -> float:
    q = stats.studentized_range.ppf(1 - alpha, k, 1e6) / math.sqrt(2)
    return float(q * math.sqrt(k * (k + 1) / (6.0 * n)))


def complete_table(experiments: pd.DataFrame, metric_name: str, min_coverage: float) -> Tuple[pd.DataFrame, List[str]]:
    table = experiments.pivot_table(index="scenario", columns="os", values=metric_name, aggfunc="median")
    if table.empty:
        return table, []
    coverage = table.notna().mean()
    dropped = sorted(coverage[coverage < min_coverage].index)
    table = table.drop(columns=dropped).dropna()
    # Still not enough complete scenarios: drop the least covered OS until there are
    while table.shape[1] > 2 and table.shape[0] < 3:
        remaining = experiments.pivot_table(index="scenario", columns="os", values=metric_name, aggfunc="median")[table.columns]
        worst = remaining.notna().mean().idxmin()
        dropped.append(worst)
        table = remaining.drop(columns=[worst]).dropna()
    return table, dropped


def rank_metric(experiments: pd.DataFrame, metric_name: str, min_coverage: float = 0.8) -> Optional[dict]:
    spec = config.metric(metric_name)
    if spec is None or metric_name not in experiments.columns:
        return None
    table, dropped = complete_table(experiments, metric_name, min_coverage)
    k, n = table.shape[1], table.shape[0]
    if k < 2 or n < 2:
        return None
    ranks = (-table if spec.higher_is_better else table).rank(axis=1, method="average")
    average = ranks.mean().sort_values()
    statistic = p_value = math.nan
    if k >= 3:
        statistic, p_value = stats.friedmanchisquare(*[table[c].to_numpy() for c in table.columns])
    elif k == 2:
        diff = table.iloc[:, 0] - table.iloc[:, 1]
        if (diff != 0).any():
            statistic, p_value = stats.wilcoxon(table.iloc[:, 0], table.iloc[:, 1])
    cd = critical_difference(k, n) if k >= 2 else math.nan
    pairs = []
    names = list(average.index)
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            gap = abs(average[a] - average[b])
            pairs.append({"os_a": a, "os_b": b, "rank_gap": gap, "significant": bool(gap > cd and (p_value < ALPHA if not math.isnan(p_value) else False))})
    wins = (ranks == 1).sum()
    return {
        "metric": metric_name,
        "k": k,
        "n_scenarios": n,
        "statistic": float(statistic),
        "p_value": float(p_value),
        "cd": cd,
        "average_ranks": average,
        "wins": wins,
        "pairs": pd.DataFrame(pairs),
        "dropped": dropped,
    }


def rank_all(experiments: pd.DataFrame, metrics: List[str], min_coverage: float = 0.8) -> Tuple[pd.DataFrame, pd.DataFrame, dict]:
    """Global ranking and one ranking per topology."""
    rows, pair_rows, results = [], [], {}
    scopes = [("all", experiments)] + [(f"topology={t}", df) for t, df in experiments.groupby("topology")] if experiments["topology"].nunique() > 1 else [("all", experiments)]
    for scope, df in scopes:
        for name in metrics:
            result = rank_metric(df, name, min_coverage)
            if result is None:
                continue
            results[(scope, name)] = result
            for os_name, rank in result["average_ranks"].items():
                rows.append(
                    {
                        "scope": scope,
                        "metric": name,
                        "os": os_name,
                        "average_rank": float(rank),
                        "wins": int(result["wins"].get(os_name, 0)),
                        "k": result["k"],
                        "n_scenarios": result["n_scenarios"],
                        "friedman_statistic": result["statistic"],
                        "p_value": result["p_value"],
                        "critical_difference": result["cd"],
                        "excluded_oses": ", ".join(result["dropped"]),
                    }
                )
            pairs = result["pairs"].copy()
            pairs.insert(0, "metric", name)
            pairs.insert(0, "scope", scope)
            pair_rows.append(pairs)
    return pd.DataFrame(rows), (pd.concat(pair_rows, ignore_index=True) if pair_rows else pd.DataFrame()), results


def cliques(average: pd.Series, cd: float) -> List[Tuple[int, int]]:
    """Maximal runs of consecutive OSes (sorted by rank) not significantly different."""
    values = average.to_numpy()
    spans = []
    for i in range(len(values)):
        j = i
        while j + 1 < len(values) and values[j + 1] - values[i] <= cd:
            j += 1
        if j > i and not any(s <= i and j <= e for s, e in spans):
            spans.append((i, j))
    return spans


def plot_cd(result: dict, path: Path, title: str) -> Path:
    """Demsar-style critical difference diagram: best ranks on the left."""
    average: pd.Series = result["average_ranks"]
    k, cd = result["k"], result["cd"]
    names = list(average.index)
    half = (len(names) + 1) // 2
    spans = cliques(average, cd)
    margin = max(1.4, 0.45 * k)
    fig, ax = plotting.plt.subplots(figsize=(8, 1.9 + 0.3 * half + 0.1 * len(spans)))
    ax.set_xlim(1 - margin, k + margin)
    axis_y, step = 0.0, 0.3
    bottom = axis_y - 0.35 - step * half - 0.12 * len(spans)
    ax.set_ylim(bottom, 0.75)
    ax.axis("off")
    ax.hlines(axis_y, 1, k, color=plotting.INK_SECONDARY, lw=1)
    for r in range(1, k + 1):
        ax.vlines(r, axis_y, axis_y + 0.08, color=plotting.INK_SECONDARY, lw=1)
        ax.text(r, axis_y + 0.12, str(r), ha="center", va="bottom", fontsize=8, color=plotting.MUTED)
    for n, (s, e) in enumerate(spans):
        y = axis_y - 0.12 - 0.12 * n
        ax.plot([average.iloc[s] - 0.04, average.iloc[e] + 0.04], [y, y], color=plotting.INK, lw=3, solid_capstyle="round", zorder=3)
    label_top = axis_y - 0.2 - 0.12 * len(spans)
    for i, os_name in enumerate(names):
        x = average[os_name]
        left = i < half
        position = i if left else len(names) - 1 - i
        y = label_top - step * position
        end = 1 - 0.25 if left else k + 0.25
        ax.plot([x, x, end], [axis_y, y, y], color=plotting.INK_SECONDARY, lw=1, zorder=2)
        ax.text(end - 0.08 if left else end + 0.08, y, f"{os_name} ({x:.2f})", ha="right" if left else "left", va="center", fontsize=8, color=plotting.INK)
    if not math.isnan(cd):
        ax.plot([1, 1 + cd], [0.5, 0.5], color=plotting.INK, lw=1.5)
        ax.text(1 + cd / 2, 0.54, f"CD = {cd:.2f}", ha="center", va="bottom", fontsize=8)
    p = result["p_value"]
    ax.set_title(f"{title}\nN = {result['n_scenarios']} scenarios, Friedman p = {p:.3g} - rank 1 = best, bars join OSes not significantly different", fontsize=9)
    return plotting.save(fig, path)


def plot_rankings(results: dict, output_dir: Path) -> List[Path]:
    paths = []
    for (scope, name), result in results.items():
        if scope != "all" or result["k"] < 3:
            continue
        spec = config.metric(name)
        paths.append(plot_cd(result, output_dir / f"cd_{name}.png", f"Critical difference - {spec.label if spec else name}"))
    return paths


SHORT_LEVELS = {"Low Resources": "LR", "Medium Resources": "MR", "High Resources": "HR", "no-failures": "no-fail", "failures": "fail"}


def short_scenarios(experiments: pd.DataFrame, scenarios: List[str]) -> List[str]:
    """Scenario labels without the factors that never vary, with short level names."""
    factors = [f for f in config.FACTORS if f != "os" and f in experiments.columns]
    varying = [i for i, f in enumerate(factors) if experiments[f].nunique() > 1]
    labels = []
    for scenario in scenarios:
        parts = scenario.split(",")
        kept = [SHORT_LEVELS.get(parts[i], parts[i]) for i in varying if i < len(parts)]
        labels.append(" | ".join(kept) or scenario)
    return labels


def plot_scoreboard(experiments: pd.DataFrame, metric_name: str, output_dir: Path) -> Optional[Path]:
    """Scenario x OS heatmap of the metric relative to the best OS of each scenario (1 = best)."""
    spec = config.metric(metric_name)
    if spec is None or metric_name not in experiments.columns:
        return None
    table = experiments.pivot_table(index="scenario", columns="os", values=metric_name, aggfunc="median")
    if table.empty:
        return None
    if spec.higher_is_better:
        score = table.div(table.max(axis=1), axis=0)
    else:
        score = 1.0 / table.div(table.min(axis=1), axis=0)
    score = score.replace([np.inf, -np.inf], np.nan).sort_index()
    score = score[score.mean().sort_values(ascending=False).index]
    rows = len(score.index)
    fig, ax = plotting.plt.subplots(figsize=(3.5 + 0.6 * score.shape[1], 1.6 + 0.2 * rows))
    image = plotting.heatmap(ax, score.to_numpy(dtype=float), short_scenarios(experiments, list(score.index)), list(score.columns), vmin=0, vmax=1, annotate=score.size <= 400)
    ax.tick_params(axis="y", labelsize=6 if rows > 40 else 7)
    fig.colorbar(image, ax=ax, label="relative to the best OS of the scenario", shrink=min(1.0, 8 / max(rows, 1)) if rows > 8 else 1.0)
    ax.set_title(f"Scoreboard - {spec.label}\n(columns: best OS on average first)", fontsize=10)
    return plotting.save(fig, output_dir / f"scoreboard_{metric_name}.png")
