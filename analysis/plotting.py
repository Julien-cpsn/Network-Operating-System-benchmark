"""Shared figure style: one palette, color follows the OS (never its rank)."""
from __future__ import annotations

from pathlib import Path
from typing import Dict, Iterable, List, Tuple

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.colors import LinearSegmentedColormap  # noqa: E402

# Categorical order (fixed): blue, orange, aqua, yellow, magenta, green, violet, red
CATEGORICAL = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#6250d6", "#e34948"]
# Beyond 8 OSes, the hue repeats with another marker and line style (composite encoding)
MARKERS = ["o", "s", "^", "D"]
LINESTYLES = ["-", "--", ":", "-."]

INK = "#0b0b0b"
INK_SECONDARY = "#52514e"
MUTED = "#898781"
GRID = "#e1e0d9"
AXIS = "#c3c2b7"
SURFACE = "#fcfcfb"

SEQUENTIAL = LinearSegmentedColormap.from_list(
    "seq_blue", ["#f4f8fd", "#cde2fb", "#9ec5f4", "#6da7ec", "#3987e5", "#256abf", "#184f95", "#0d366b"]
)
DIVERGING = LinearSegmentedColormap.from_list("div_blue_orange", ["#184f95", "#6da7ec", "#f0efec", "#ec835a", "#b8431c"])

plt.rcParams.update(
    {
        "figure.facecolor": SURFACE,
        "axes.facecolor": SURFACE,
        "savefig.facecolor": SURFACE,
        "axes.edgecolor": AXIS,
        "axes.labelcolor": INK_SECONDARY,
        "axes.titlecolor": INK,
        "axes.titlesize": 11,
        "axes.labelsize": 9,
        "axes.grid": True,
        "grid.color": GRID,
        "grid.linewidth": 0.6,
        "axes.spines.top": False,
        "axes.spines.right": False,
        "xtick.color": MUTED,
        "ytick.color": MUTED,
        "xtick.labelsize": 8,
        "ytick.labelsize": 8,
        "legend.fontsize": 8,
        "legend.frameon": False,
        "lines.linewidth": 2,
        "lines.markersize": 6,
        "font.size": 9,
    }
)


class OsStyle:
    """Stable color/marker per OS, from the sorted list of every OS in the dataset."""

    def __init__(self, oses: Iterable[str]):
        self.order: List[str] = sorted(set(str(o) for o in oses))
        self.index: Dict[str, int] = {os_name: i for i, os_name in enumerate(self.order)}

    def color(self, os_name: str) -> str:
        return CATEGORICAL[self.index.get(str(os_name), 0) % len(CATEGORICAL)]

    def marker(self, os_name: str) -> str:
        return MARKERS[(self.index.get(str(os_name), 0) // len(CATEGORICAL)) % len(MARKERS)]

    def linestyle(self, os_name: str) -> str:
        return LINESTYLES[(self.index.get(str(os_name), 0) // len(CATEGORICAL)) % len(LINESTYLES)]

    def kwargs(self, os_name: str) -> dict:
        return {"color": self.color(os_name), "marker": self.marker(os_name), "linestyle": self.linestyle(os_name), "label": os_name}


def grid(n: int, columns: int = 3, size: Tuple[float, float] = (4.2, 3.2)):
    columns = max(1, min(columns, n))
    rows = (n + columns - 1) // columns
    fig, axes = plt.subplots(rows, columns, figsize=(size[0] * columns, size[1] * rows), squeeze=False)
    flat = list(axes.flat)
    for ax in flat[n:]:
        ax.set_visible(False)
    return fig, flat[:n]


def heatmap(ax, matrix, row_labels, column_labels, cmap=SEQUENTIAL, vmin=None, vmax=None, fmt="{:.2f}", annotate=True):
    image = ax.imshow(matrix, cmap=cmap, vmin=vmin, vmax=vmax, aspect="auto")
    ax.set_xticks(range(len(column_labels)))
    ax.set_xticklabels(column_labels, rotation=45, ha="right")
    ax.set_yticks(range(len(row_labels)))
    ax.set_yticklabels(row_labels)
    ax.grid(False)
    if annotate and matrix.size <= 600:
        low = vmin if vmin is not None else float(matrix[~_isnan(matrix)].min()) if (~_isnan(matrix)).any() else 0
        high = vmax if vmax is not None else float(matrix[~_isnan(matrix)].max()) if (~_isnan(matrix)).any() else 1
        span = (high - low) or 1.0
        for (i, j), value in _enumerate(matrix):
            if value != value:  # NaN
                continue
            dark = (value - low) / span > 0.6 if cmap is SEQUENTIAL else abs(value) > 0.6 * max(abs(low), abs(high))
            ax.text(j, i, fmt.format(value), ha="center", va="center", fontsize=7, color="#ffffff" if dark else INK)
    return image


def _isnan(matrix):
    import numpy as np

    return np.isnan(matrix)


def _enumerate(matrix):
    import numpy as np

    return np.ndenumerate(matrix)


def log_axis(ax, axis: str = "y") -> None:
    """Log scale with plain tick labels (1000, 2000 rather than 10^3, 2x10^3)."""
    from matplotlib.ticker import FuncFormatter

    formatter = FuncFormatter(lambda v, _: f"{v:,.0f}" if v >= 10 else f"{v:g}")
    if axis == "y":
        ax.set_yscale("log")
        ax.yaxis.set_major_formatter(formatter)
        ax.yaxis.set_minor_formatter(FuncFormatter(lambda v, _: ""))
    else:
        ax.set_xscale("log")
        ax.xaxis.set_major_formatter(formatter)


def save(fig, path: Path) -> Path:
    import warnings

    path.parent.mkdir(parents=True, exist_ok=True)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)
    return path


def slug(text: str) -> str:
    import re

    return re.sub(r"[^A-Za-z0-9]+", "_", str(text)).strip("_").lower() or "all"
