"""Step 6 - trade-offs: Pareto fronts, metric correlations, sensitivity to each factor.

- ``pareto.csv`` / ``pareto_share.csv``: in each scenario, the OSes that no other OS beats on
  every objective at once (throughput up, p95 latency down, router cores down). An OS often
  on the front is a candidate "good routing OS" whatever the weights given to the objectives.
- ``correlations.csv``: Spearman correlation between metrics (over the experiment medians):
  which metrics move together, which are redundant.
- ``sensitivity.csv``: per OS, how a metric changes from the first to the last level of an
  ordered factor, everything else equal (median ratio over the scenarios):
  ``resources`` High/Low = scaling with the vCPUs (4x the vCPUs: 4.0 would be linear),
  ``nic`` VirtIO/RTL8139 = dependence on paravirtualisation, ``failures`` = cost of a failure.
"""
from __future__ import annotations

from pathlib import Path
from typing import List, Optional

import numpy as np
import pandas as pd
from scipy import stats

from analysis import config, plotting

SENSITIVITY_FACTORS = ["resources", "nic", "failures", "routing"]


# --------------------------------------------------------------------------- Pareto


def dominated(values: np.ndarray, higher: List[bool]) -> np.ndarray:
    """values: (n, m). Returns a boolean mask of the dominated rows."""
    signed = np.where(np.asarray(higher), values, -values)
    out = np.zeros(len(values), dtype=bool)
    for i in range(len(values)):
        better_eq = np.all(signed >= signed[i], axis=1)
        strictly = np.any(signed > signed[i], axis=1)
        out[i] = bool(np.any(better_eq & strictly))
    return out


def pareto(experiments: pd.DataFrame, objectives: List[str] = None) -> pd.DataFrame:
    objectives = [o for o in (objectives or config.PARETO_OBJECTIVES) if o in experiments.columns]
    if len(objectives) < 2:
        return pd.DataFrame()
    higher = [config.metric(o).higher_is_better for o in objectives]
    rows = []
    for scenario, group in experiments.dropna(subset=objectives).groupby("scenario"):
        if group["os"].nunique() < 2:
            continue
        mask = dominated(group[objectives].to_numpy(dtype=float), higher)
        for (_, row), is_dominated in zip(group.iterrows(), mask):
            rows.append({"scenario": scenario, "topology": row["topology"], "os": row["os"], **{o: row[o] for o in objectives}, "on_front": not is_dominated})
    return pd.DataFrame(rows)


def pareto_share(front: pd.DataFrame) -> pd.DataFrame:
    if front.empty:
        return front
    grouped = front.groupby("os")["on_front"]
    return pd.DataFrame({"scenarios": grouped.size(), "on_front": grouped.sum(), "share_on_front": grouped.mean()}).sort_values("share_on_front", ascending=False).reset_index()


def plot_pareto(front: pd.DataFrame, output_dir: Path, style: plotting.OsStyle, objectives: List[str] = None) -> Optional[Path]:
    objectives = [o for o in (objectives or config.PARETO_OBJECTIVES) if o in front.columns]
    if front.empty or len(objectives) < 2:
        return None
    x_name, y_name = objectives[0], objectives[1]
    data = front.copy()
    # Normalised per scenario so that every scenario fits on the same axes: 1 = best
    data["x"] = data[x_name] / data.groupby("scenario")[x_name].transform("max")
    data["y"] = data[y_name] / data.groupby("scenario")[y_name].transform("min")
    topologies = sorted(data["topology"].unique())
    fig, axes = plotting.grid(len(topologies), columns=min(3, len(topologies)), size=(4.6, 3.8))
    for ax, topology in zip(axes, topologies):
        subset = data[data["topology"] == topology]
        for os_name in style.order:
            points = subset[subset["os"] == os_name]
            if points.empty:
                continue
            color = style.color(os_name)
            marker = style.marker(os_name)
            front_points = points[points["on_front"]]
            other = points[~points["on_front"]]
            ax.scatter(other["x"], other["y"], s=28, facecolors="none", edgecolors=color, marker=marker, linewidths=1)
            ax.scatter(front_points["x"], front_points["y"], s=36, color=color, marker=marker, edgecolors=plotting.SURFACE, linewidths=1.5, label=os_name)
        plotting.log_axis(ax)
        ax.set_xlabel(f"{x_name} / best of the scenario")
        ax.set_ylabel(f"{y_name} / best of the scenario")
        ax.set_title(f"{topology} (filled = on the Pareto front)", fontsize=9)
    axes[0].legend(loc="best")
    return plotting.save(fig, output_dir / "pareto.png")


# --------------------------------------------------------------------------- correlations


def correlations(experiments: pd.DataFrame, min_pairs: int = 10) -> pd.DataFrame:
    metrics = [m.name for m in config.METRICS if m.name in experiments.columns and experiments[m.name].notna().sum() >= min_pairs]
    rows = []
    for i, a in enumerate(metrics):
        for b in metrics[i + 1:]:
            pair = experiments[[a, b]].dropna()
            if len(pair) < min_pairs or pair[a].nunique() < 2 or pair[b].nunique() < 2:
                continue
            rho, p = stats.spearmanr(pair[a], pair[b])
            rows.append({"metric_a": a, "metric_b": b, "spearman_rho": float(rho), "p_value": float(p), "n": len(pair)})
    return pd.DataFrame(rows)


def plot_correlations(table: pd.DataFrame, output_dir: Path) -> Optional[Path]:
    if table.empty:
        return None
    names = [m.name for m in config.METRICS if m.name in set(table["metric_a"]) | set(table["metric_b"])]
    matrix = np.full((len(names), len(names)), np.nan)
    np.fill_diagonal(matrix, 1.0)
    for _, row in table.iterrows():
        i, j = names.index(row["metric_a"]), names.index(row["metric_b"])
        matrix[i, j] = matrix[j, i] = row["spearman_rho"]
    size = 2 + 0.42 * len(names)
    fig, ax = plotting.plt.subplots(figsize=(size + 1.5, size))
    image = plotting.heatmap(ax, matrix, names, names, cmap=plotting.DIVERGING, vmin=-1, vmax=1, fmt="{:.1f}")
    fig.colorbar(image, ax=ax, label="Spearman ρ")
    ax.set_title("Correlation between metrics (experiment medians)")
    return plotting.save(fig, output_dir / "correlations.png")


# --------------------------------------------------------------------------- sensitivity


def sensitivity(experiments: pd.DataFrame, metrics: List[str], factors: List[str] = None) -> pd.DataFrame:
    rows = []
    for factor in factors or SENSITIVITY_FACTORS:
        if factor not in experiments.columns or experiments[factor].nunique() < 2:
            continue
        levels = config.order_levels(factor, experiments[factor].unique())
        low, high = levels[0], levels[-1]
        context = [f for f in config.FACTORS if f not in ("os", factor) and f in experiments.columns]
        for name in metrics:
            if name not in experiments.columns:
                continue
            table = experiments.pivot_table(index=["os", *context], columns=factor, values=name, aggfunc="median")
            if low not in table.columns or high not in table.columns:
                continue
            pair = table[[low, high]].dropna()
            pair = pair[(pair[low] > 0) & (pair[high] > 0)] if config.metric(name).log else pair
            if pair.empty:
                continue
            if config.metric(name).log:
                change = pair[high] / pair[low]
                kind = "ratio"
            else:
                change = pair[high] - pair[low]
                kind = "difference"
            for os_name, values in change.groupby(level="os"):
                rows.append(
                    {
                        "factor": factor,
                        "from": low,
                        "to": high,
                        "metric": name,
                        "os": os_name,
                        "kind": kind,
                        "median_change": float(values.median()),
                        "q1": float(values.quantile(0.25)),
                        "q3": float(values.quantile(0.75)),
                        "contexts": int(len(values)),
                    }
                )
    return pd.DataFrame(rows)


def plot_sensitivity(table: pd.DataFrame, output_dir: Path) -> List[Path]:
    paths = []
    if table.empty:
        return paths
    for (factor, name), part in table.groupby(["factor", "metric"]):
        if name not in ("tcp_total_mbps", "icmp_latency_p95_ms", "routers_guest_cores", "mbps_per_guest_core"):
            continue
        part = part.sort_values("median_change")
        fig, ax = plotting.plt.subplots(figsize=(6, 0.9 + 0.32 * len(part)))
        err = np.vstack([part["median_change"] - part["q1"], part["q3"] - part["median_change"]])
        ax.barh(part["os"], part["median_change"], xerr=err, color=plotting.CATEGORICAL[0], height=0.6, error_kw={"ecolor": plotting.MUTED, "lw": 1})
        reference = 1.0 if part["kind"].iloc[0] == "ratio" else 0.0
        ax.axvline(reference, color=plotting.INK_SECONDARY, lw=1)
        ax.set_xlabel(f"{name}: {part['to'].iloc[0]} {'÷' if reference else '−'} {part['from'].iloc[0]} (median, IQR)")
        ax.grid(axis="y", visible=False)
        ax.set_title(f"Sensitivity to {factor}")
        paths.append(plotting.save(fig, output_dir / f"sensitivity_{factor}_{name}.png"))
    return paths
