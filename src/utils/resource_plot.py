#!python
import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.legend_handler import HandlerTuple
from matplotlib.patches import Ellipse


def extract_summary(path: str):
    """Reads a summary written by `compute results` (one CSV per router, merged over the runs).

    Returns the median series, the min/max band across runs and the end of the experiment
    (in seconds since its start), or None if the file holds no data.
    """
    columns = ["elapsed_s", "mem_used_mib_median", "mem_used_mib_min", "mem_used_mib_max", "cpu_pct_median", "cpu_pct_min", "cpu_pct_max"]
    series = {c: [] for c in columns}
    in_experiment = []

    def number(value: str) -> float:
        return float(value) if value != "" else float("nan")

    with open(path, "r", encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            for c in columns:
                series[c].append(number(row[c]))
            in_experiment.append(row["in_experiment"] == "true")

    if not series["elapsed_s"]:
        return None

    inside = [t for t, flag in zip(series["elapsed_s"], in_experiment) if flag]
    return {
        "elapsed": series["elapsed_s"],
        "mem": series["mem_used_mib_median"],
        "mem_band": (series["mem_used_mib_min"], series["mem_used_mib_max"]),
        "cpu": series["cpu_pct_median"],
        "cpu_band": (series["cpu_pct_min"], series["cpu_pct_max"]),
        "end": max(inside) if inside else None,
    }

def main(output_path: str, inputs: list[str]):
    fig, ax1 = plt.subplots(figsize=(7, 4), dpi=150)
    ax2 = ax1.twinx()

    ellipse_fig, ellipse_ax = plt.subplots(figsize=(6, 5), dpi=150)
    all_cpu_values = []


    mem_lines = []
    cpu_lines = []

    experiment_end_elapsed = None
    has_band = False

    for input in inputs:
        data = input.split(":")

        if len(data) == 1:
            path = data[0]
            label = Path(path).stem
        else:
            label = data[0]
            path = data[1]

        resources = extract_summary(path)

        if resources is None:
            print(f"Warning: no data found in {path}")
            continue

        elapsed = resources["elapsed"]
        used_memory = resources["mem"]
        cpu_usage = resources["cpu"]

        if experiment_end_elapsed is None:
            experiment_end_elapsed = resources["end"]

        mem_line, = ax1.plot(elapsed, used_memory, label=label)
        cpu_line, = ax2.plot(elapsed, cpu_usage, linestyle=":", color=mem_line.get_color())

        # Spread across the runs (summaries only): min-max band around the median.
        if resources["mem_band"] is not None:
            has_band = True
            ax1.fill_between(elapsed, *resources["mem_band"], color=mem_line.get_color(), alpha=0.15, linewidth=0)
            ax2.fill_between(elapsed, *resources["cpu_band"], color=mem_line.get_color(), alpha=0.08, linewidth=0)

        mem_lines.append(mem_line)
        cpu_lines.append(cpu_line)

        # --------------------------------------------------------------
        # CPU / memory ellipse
        # --------------------------------------------------------------
        cpu_min = min(cpu_usage)
        cpu_max = max(cpu_usage)
        mem_min = min(used_memory)
        mem_max = max(used_memory)

        cpu_center = (cpu_min + cpu_max) / 2
        mem_center = (mem_min + mem_max) / 2

        cpu_range = cpu_max - cpu_min
        mem_range = mem_max - mem_min

        all_cpu_values.extend(cpu_usage)

        ellipse = Ellipse(
            xy=(cpu_center, mem_center),
            width=cpu_range,
            height=mem_range,
            angle=0,
            facecolor=mem_line.get_color(),
            edgecolor=mem_line.get_color(),
            alpha=0.20,
            linewidth=1.5,
            label=label
        )

        ellipse_ax.add_patch(ellipse)

        # Mark the center of the min/max range
        ellipse_ax.plot(cpu_center, mem_center, marker=".", color=mem_line.get_color())


    if not mem_lines:
        print("Error: no data to plot")
        sys.exit(1)

    # ------------------------------------------------------------------
    # Experiment boundaries
    # ------------------------------------------------------------------

    # Experiment boundaries
    ax1.axvline(x=0, color="grey", linestyle="--", linewidth=1)
    ax1.axvline(x=experiment_end_elapsed, color="grey", linestyle="--", linewidth=1)

    ax1.text(0, 0.6, "t0", transform=ax1.get_xaxis_transform(), rotation=90, va="top", ha="right", color="grey")
    ax1.text(experiment_end_elapsed + 1, 0.6, "t_end", transform=ax1.get_xaxis_transform(), rotation=90, va="top", ha="left", color="grey")

    # ------------------------------------------------------------------
    # Time-series plot formatting
    # ------------------------------------------------------------------

    ax1.set_xlabel("Time (seconds since experiment start)")
    ax1.set_ylabel("— Used Memory (MiB, guest-measured)")
    # Since the host monitor, cpu_pct is the guest execution time measured on the host,
    # in % of the router's vCPUs (column "cpu_source" of the summaries)
    ax2.set_ylabel("···  CPU load (% of vCPUs, host-measured)")

    ax1.set_ylim(bottom=0)
    ax2.set_ylim(0, 100)

    plt.title("Used Memory and CPU load")

    # Note: the former `plt.title("Used Memory and CPU load")` acted on the last created figure
    # (the ellipse plot) and was then overwritten, so it never showed on this plot.
    if has_band:
        ax1.set_title("Median over runs (shaded: min to max)", fontsize=9)


    # Combine legends
    legend_handles = [
        (m, c) for m, c in zip(mem_lines, cpu_lines)
    ]
    legend_labels = [m.get_label() for m in mem_lines]

    max_label = len(max(legend_labels, key=len))

    ax1.legend(
        legend_handles,
        legend_labels,
        handler_map={tuple: HandlerTuple(ndivide=None)},
        loc="center left",
        bbox_to_anchor=(1.17, 0.85 - len(legend_labels) * 0.02, 0.0, 0.0),
        borderaxespad=0,
        frameon=False
    )

    fig.tight_layout(rect=(0, 0, 1.0 + 0.0035 * max_label, 1))

    # ------------------------------------------------------------------
    # Ellipse plot formatting
    # ------------------------------------------------------------------

    ellipse_ax.set_xlabel("CPU load (%)")
    ellipse_ax.set_ylabel("Used Memory (MiB)")
    ellipse_ax.set_title("CPU load vs. Used Memory")

    # Adaptive CPU axis based on the actual data.
    cpu_min_all = min(all_cpu_values)
    cpu_max_all = max(all_cpu_values)

    cpu_range = cpu_max_all - cpu_min_all
    cpu_margin = max(cpu_range * 0.05, 1.0)

    ellipse_ax.set_xlim(
        max(0, cpu_min_all - cpu_margin),
        cpu_max_all + cpu_margin
    )
    ellipse_ax.set_ylim(bottom=0)

    # Use the experiment names for the ellipse legend
    ellipse_ax.legend(loc="center left", bbox_to_anchor=(1.0, 0.85), frameon=False )
    ellipse_ax.grid(True, alpha=0.2)

    ellipse_fig.tight_layout()

    # ------------------------------------------------------------------
    # Save both plots
    # ------------------------------------------------------------------
    output_path = Path(output_path)

    fig.savefig(Path(output_path).with_suffix('.svg'))
    fig.savefig(Path(output_path).with_suffix('.png'))
    ellipse_fig.savefig(output_path.with_name(output_path.stem + "_ellipse.svg"))
    ellipse_fig.savefig(output_path.with_name(output_path.stem + "_ellipse.png"))


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <output_path> <log1> <name:log2> <test:log3> ...")
        sys.exit(1)

    main(sys.argv[1], sys.argv[2:])