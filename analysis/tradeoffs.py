"""Step 6 - trade-offs: Pareto fronts, metric correlations, sensitivity to each factor.

- ``pareto.csv`` / ``pareto_share.csv``: in each scenario, the OSes that no other OS beats on
  every objective at once (throughput up, p95 latency down, router cores down). An OS often
  on the front is a candidate "good routing OS" whatever the weights given to the objectives.
- ``pareto_os_frontiers.csv``: per topology, the frontier of each OS alone (throughput vs p95
  latency, over every scenario of the topology): the best trade-offs it reaches, drawn as a
  line in ``pareto.png``. A frontier entirely below-right of another one is a better OS.
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


def frontier_2d(x: np.ndarray, y: np.ndarray, higher_x: bool = True, higher_y: bool = False) -> np.ndarray:
    """Indices of the non-dominated points of a 2D cloud, sorted by increasing x."""
    sx = np.where(higher_x, x, -x)
    sy = np.where(higher_y, y, -y)
    order = np.lexsort((-sy, -sx))  # best x first, ties broken by best y
    kept, best_y = [], -np.inf
    for i in order:
        if sy[i] > best_y:
            kept.append(i)
            best_y = sy[i]
    kept = np.asarray(kept, dtype=int)
    return kept[np.argsort(x[kept])]


def os_frontiers(front: pd.DataFrame, objectives: List[str] = None) -> pd.DataFrame:
    """Per topology and OS, the Pareto frontier of that OS alone on the first two objectives
    (absolute values): the best trade-offs the OS reaches over all the scenarios of the
    topology (resources, NIC, routing, ...)."""
    objectives = [o for o in (objectives or config.PARETO_OBJECTIVES) if o in front.columns]
    if front.empty or len(objectives) < 2:
        return pd.DataFrame()
    x_name, y_name = objectives[0], objectives[1]
    higher = [config.metric(o).higher_is_better for o in (x_name, y_name)]
    rows = []
    for (topology, os_name), points in front.dropna(subset=[x_name, y_name]).groupby(["topology", "os"]):
        points = points.reset_index(drop=True)
        for rank, i in enumerate(frontier_2d(points[x_name].to_numpy(float), points[y_name].to_numpy(float), *higher)):
            row = points.loc[i]
            rows.append({"topology": topology, "os": os_name, "order": rank, "scenario": row["scenario"], x_name: row[x_name], y_name: row[y_name]})
    return pd.DataFrame(rows)


def plot_pareto(front: pd.DataFrame, output_dir: Path, style: plotting.OsStyle, objectives: List[str] = None) -> Optional[Path]:
    """Per topology: every experiment (filled = on the Pareto front of its scenario, over all the
    objectives), and the frontier of each OS (its best trade-offs in this plane, as a staircase)."""
    objectives = [o for o in (objectives or config.PARETO_OBJECTIVES) if o in front.columns]
    if front.empty or len(objectives) < 2:
        return None
    from matplotlib.lines import Line2D

    x_name, y_name = objectives[0], objectives[1]
    higher_x = config.metric(x_name).higher_is_better
    data = front.rename(columns={x_name: "x", y_name: "y"})
    frontiers = os_frontiers(front, objectives).rename(columns={x_name: "x", y_name: "y"})
    topologies = sorted(data["topology"].unique())
    fig, axes = plotting.grid(len(topologies), columns=min(3, len(topologies)), size=(4.8, 4.0))
    handles = {}
    for ax, topology in zip(axes, topologies):
        subset = data[data["topology"] == topology]
        for os_name in style.order:
            points = subset[subset["os"] == os_name]
            if points.empty:
                continue
            color, marker = style.color(os_name), style.marker(os_name)
            front_points = points[points["on_front"]]
            other = points[~points["on_front"]]
            ax.scatter(other["x"], other["y"], s=26, facecolors="none", edgecolors=color, marker=marker, linewidths=1, alpha=0.55, zorder=2)
            ax.scatter(front_points["x"], front_points["y"], s=36, color=color, marker=marker, edgecolors=plotting.SURFACE, linewidths=1.5, zorder=4)
            line = frontiers[(frontiers["topology"] == topology) & (frontiers["os"] == os_name)].sort_values("x")
            if len(line):
                # Staircase: between two frontier points, the best y reachable is the next point's
                ax.step(line["x"], line["y"], where="pre" if higher_x else "post", color=color, linestyle=style.linestyle(os_name), lw=2, zorder=3)
            handles[os_name] = Line2D([], [], color=color, marker=marker, linestyle=style.linestyle(os_name), lw=2, markeredgecolor=plotting.SURFACE, label=os_name)
        plotting.log_axis(ax, "x")
        plotting.log_axis(ax, "y")
        ax.set_xlabel(config.metric(x_name).label)
        ax.set_ylabel(config.metric(y_name).label)
        ax.set_title(topology, fontsize=10)
    ordered = [handles[o] for o in style.order if o in handles]
    fig.legend(handles=ordered, loc="lower center", ncol=min(len(ordered), 6), bbox_to_anchor=(0.5, -0.02))
    fig.suptitle(
        "Lines: Pareto frontier of each OS (its best trade-offs over the scenarios of the topology).\n"
        f"Filled: on the Pareto front of its scenario ({', '.join(objectives)}); hollow: dominated.",
        fontsize=9,
        color=plotting.INK_SECONDARY,
    )
    import warnings

    path = output_dir / "pareto.png"
    path.parent.mkdir(parents=True, exist_ok=True)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        fig.tight_layout(rect=(0, 0.05 + 0.03 * ((len(ordered) - 1) // 6), 1, 0.95))
    fig.savefig(path, dpi=150)
    plotting.plt.close(fig)
    return path


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
