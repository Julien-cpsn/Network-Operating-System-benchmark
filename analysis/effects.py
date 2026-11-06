"""Step 4 - how much does each factor matter? Effect sizes from linear models.

Per block (topology) and response, on run-level data:
``y ~ os + resources + nic + ... + os:resources + os:nic + ...`` (sum-to-zero contrasts,
``log(y)`` for multiplicative metrics such as throughput, latency and resources), then a
type II ANOVA. For each term:
- ``partial_eta2`` = SS / (SS + SS_residual);
- ``partial_omega2`` = (SS - df * MS_residual) / (SS + (N - df) * MS_residual), less biased;
- ``share_ss`` = SS / SS_total, read as "the term explains X % of the variation".

The ``os:<factor>`` interactions carry the architectural story: does the OS scale with the
vCPUs, how much does it suffer from emulated NICs, does it degrade with more hops.

A second "architecture" model replaces ``os`` by ``os_family``, ``network_stack`` and
``routing_stack`` (nested in ``os``, hence never in the same model).
"""
from __future__ import annotations

import logging
import math
import warnings
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np
import pandas as pd
import statsmodels.formula.api as smf
from statsmodels.stats.anova import anova_lm

from analysis import config, design, plotting

LOG = logging.getLogger("analysis.effects")
MIN_RESIDUAL_DF = 3


def transform(values: pd.Series, name: str) -> pd.Series:
    spec = config.metric(name)
    values = pd.to_numeric(values, errors="coerce")
    if spec is not None and spec.log:
        positive = values[values > 0]
        if positive.empty:
            return values * np.nan
        floor = positive.min() / 10.0
        return np.log(values.clip(lower=floor))
    return values


def _formula(factors: List[str], interactions: List[str]) -> str:
    terms = [f"C({f}, Sum)" for f in factors]
    terms += [":".join(f"C({p}, Sum)" for p in term.split(":")) for term in interactions]
    return "y ~ " + " + ".join(terms)


def _term_label(term: str) -> str:
    return term.replace("C(", "").replace(", Sum)", "")


def fit(df: pd.DataFrame, response: str, factors: List[str], interactions: List[str]) -> Optional[pd.DataFrame]:
    data = df[factors + [response]].copy()
    data["y"] = transform(data[response], response)
    data = data.dropna(subset=["y"])
    factors = [f for f in factors if data[f].nunique() > 1]
    interactions = [t for t in interactions if all(p in factors for p in t.split(":")) and design.complete_interaction(data, *t.split(":"))]
    if not factors or len(data) < 8:
        return None
    if data["y"].std() == 0:
        return None

    for attempt in (interactions, []):
        formula = _formula(factors, attempt)
        try:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                model = smf.ols(formula, data=data).fit()
                if model.df_resid < MIN_RESIDUAL_DF:
                    continue
                table = anova_lm(model, typ=2)
        except (ValueError, np.linalg.LinAlgError) as error:
            LOG.debug("%s: %s failed (%s)", response, formula, error)
            continue
        table = table.rename(columns={"sum_sq": "ss", "PR(>F)": "p_value"})
        residual = table.loc["Residual"]
        ms_res = residual["ss"] / residual["df"]
        total_ss = float(((data["y"] - data["y"].mean()) ** 2).sum())
        rows = []
        for term, line in table.drop(index="Residual").iterrows():
            ss, dof = float(line["ss"]), float(line["df"])
            rows.append(
                {
                    "term": _term_label(term),
                    "df": dof,
                    "ss": ss,
                    "F": float(line["F"]),
                    "p_value": float(line["p_value"]),
                    "partial_eta2": ss / (ss + residual["ss"]) if ss + residual["ss"] > 0 else math.nan,
                    "partial_omega2": max(0.0, (ss - dof * ms_res) / (ss + (len(data) - dof) * ms_res)) if ss + (len(data) - dof) * ms_res > 0 else math.nan,
                    "share_ss": ss / total_ss if total_ss > 0 else math.nan,
                }
            )
        out = pd.DataFrame(rows)
        out["n_obs"] = len(data)
        out["r2"] = float(model.rsquared)
        out["interactions_dropped"] = len(attempt) < len(interactions)
        return out
    return None


def effect_sizes(runs: pd.DataFrame, experiments: pd.DataFrame, responses: List[str]) -> pd.DataFrame:
    tables = []
    models = {
        "os": ("os", ["os"] + [f for f in config.FACTORS if f not in ("os", design.BLOCK_FACTOR)]),
        "architecture": ("network_stack", config.ARCHITECTURE_FACTORS + [f for f in config.FACTORS if f not in ("os", design.BLOCK_FACTOR)]),
    }
    for model_name, (main, candidates) in models.items():
        candidates = [c for c in candidates if c in experiments.columns]
        for block in design.blocks(experiments, main=main, candidates=candidates):
            block_runs = runs[runs[design.BLOCK_FACTOR] == block.name]
            for response in responses:
                if response not in block_runs.columns or block_runs[response].notna().sum() < 8:
                    continue
                table = fit(block_runs, response, block.factors, block.interactions)
                if table is None:
                    continue
                table.insert(0, "response", response)
                table.insert(0, "block", block.name)
                table.insert(0, "model", model_name)
                tables.append(table)
    return pd.concat(tables, ignore_index=True) if tables else pd.DataFrame()


def plot_effects(effects: pd.DataFrame, output_dir: Path) -> List[Path]:
    paths = []
    if effects.empty:
        return paths
    for (model_name, block), table in effects.groupby(["model", "block"]):
        pivot = table.pivot_table(index="term", columns="response", values="partial_omega2", aggfunc="first")
        responses = [r for r in config.KEY_METRICS if r in pivot.columns] + [r for r in pivot.columns if r not in config.KEY_METRICS]
        pivot = pivot[responses]
        pivot = pivot.loc[pivot.max(axis=1).sort_values(ascending=False).index]
        fig, ax = plotting.plt.subplots(figsize=(1.0 + 0.75 * len(pivot.columns), 1.2 + 0.35 * len(pivot.index)))
        image = plotting.heatmap(ax, pivot.to_numpy(dtype=float), list(pivot.index), list(pivot.columns), vmin=0, vmax=1)
        fig.colorbar(image, ax=ax, label="partial ω²")
        ax.set_title(f"Effect sizes - {block} ({model_name} model)")
        paths.append(plotting.save(fig, output_dir / f"effects_{model_name}_{plotting.slug(block)}.png"))
    return paths


def plot_interactions(experiments: pd.DataFrame, output_dir: Path, responses: List[str], style: plotting.OsStyle) -> List[Path]:
    """One figure per block and response: one panel per factor, one line per OS
    (median over the units of the other factors)."""
    paths = []
    for block_name, df in experiments.groupby(design.BLOCK_FACTOR):
        others = [f for f in design.varying(df, config.FACTORS) if f not in ("os", design.BLOCK_FACTOR)]
        if not others or df["os"].nunique() < 1:
            continue
        for response in responses:
            if response not in df.columns or df[response].notna().sum() < 4:
                continue
            spec = config.metric(response)
            fig, axes = plotting.grid(len(others), columns=min(3, len(others)))
            for ax, factor in zip(axes, others):
                levels = config.order_levels(factor, df[factor].unique())
                summary = df.groupby(["os", factor])[response].median()
                for os_name in style.order:
                    if os_name not in df["os"].unique():
                        continue
                    ys = [summary.get((os_name, level), np.nan) for level in levels]
                    ax.plot(range(len(levels)), ys, **style.kwargs(os_name))
                ax.set_xticks(range(len(levels)))
                ax.set_xticklabels(levels, rotation=20, ha="right")
                ax.set_xlabel(factor)
                ax.set_ylabel(spec.label if spec else response)
                if spec is not None and spec.log and (df[response] > 0).all():
                    plotting.log_axis(ax)
            axes[0].legend(loc="best", ncol=1 if df["os"].nunique() <= 6 else 2)
            fig.suptitle(f"{block_name} - {spec.label if spec else response}: OS x factor", color=plotting.INK)
            paths.append(plotting.save(fig, output_dir / f"interactions_{plotting.slug(block_name)}_{response}.png"))
    return paths


def top_effects(effects: pd.DataFrame, n: int = 5) -> Dict[tuple, pd.DataFrame]:
    out = {}
    if effects.empty:
        return out
    for key, table in effects[effects["model"] == "os"].groupby(["block", "response"]):
        out[key] = table.sort_values("partial_omega2", ascending=False).head(n)
    return out
