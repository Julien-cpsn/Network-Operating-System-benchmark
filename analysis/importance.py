"""Step 4b - model-based cross-check: gradient boosting, permutation importance, H-statistic.

The linear model assumes effects that add up (on the log scale). A gradient boosting model
does not: if both approaches rank the factors the same way, the conclusion is solid.

- ``importance.csv``: per response, the drop of the out-of-fold R² when a factor is shuffled
  (permutation importance, folds grouped by unit so the runs of an experiment never sit on
  both sides), with the cross-validated R² of the model (low R² = do not trust the ranking).
- ``interactions_h.csv``: Friedman's H² for every pair of factors: the share of their joint
  effect that is an interaction (0 = additive, 1 = pure interaction), with the share of the
  prediction variance the pair accounts for (``joint_effect_share``, ignore H² when tiny).
"""
from __future__ import annotations

import logging
from itertools import combinations
from pathlib import Path
from typing import List, Tuple

import numpy as np
import pandas as pd
from sklearn.ensemble import HistGradientBoostingRegressor
from sklearn.inspection import permutation_importance
from sklearn.metrics import r2_score
from sklearn.model_selection import GroupKFold

from analysis import config, plotting
from analysis.effects import transform

LOG = logging.getLogger("analysis.importance")
MIN_UNITS = 12
H_SAMPLE = 300
# Below this share of the prediction variance, a pair's joint effect is negligible
MIN_JOINT_SHARE = 0.02


def _encode(df: pd.DataFrame, factors: List[str]) -> Tuple[np.ndarray, List[List[str]]]:
    levels = [sorted(df[f].astype(str).unique()) for f in factors]
    columns = [df[f].astype(str).map({level: i for i, level in enumerate(lv)}).to_numpy() for f, lv in zip(factors, levels)]
    return np.column_stack(columns).astype(float), levels


def _model(n_features: int) -> HistGradientBoostingRegressor:
    return HistGradientBoostingRegressor(
        categorical_features=[True] * n_features,
        max_iter=300,
        learning_rate=0.05,
        min_samples_leaf=5,
        l2_regularization=1.0,
        random_state=0,
    )


def h_statistic(model, X: np.ndarray, j: int, k: int, levels_j: int, levels_k: int) -> Tuple[float, float]:
    """Friedman's H² of features j and k, on categorical features (exact partial dependence
    over the observed levels), and the share of the prediction variance their joint effect
    represents. H² is only meaningful when that share is not negligible: for two factors with
    no effect, H² is the ratio of two noises."""

    def pd_of(assign):
        Xc = X.copy()
        for column, value in assign:
            Xc[:, column] = value
        return model.predict(Xc).mean()

    pd_j = {a: pd_of([(j, a)]) for a in range(levels_j)}
    pd_k = {b: pd_of([(k, b)]) for b in range(levels_k)}
    pd_jk = {(a, b): pd_of([(j, a), (k, b)]) for a in range(levels_j) for b in range(levels_k)}
    mean_j, mean_k = np.mean([pd_j[int(a)] for a in X[:, j]]), np.mean([pd_k[int(b)] for b in X[:, k]])
    mean_jk = np.mean([pd_jk[(int(a), int(b))] for a, b in zip(X[:, j], X[:, k])])
    num = den = 0.0
    for a, b in zip(X[:, j].astype(int), X[:, k].astype(int)):
        joint = pd_jk[(a, b)] - mean_jk
        num += (joint - (pd_j[a] - mean_j) - (pd_k[b] - mean_k)) ** 2
        den += joint ** 2
    predictions = model.predict(X)
    total = float(np.sum((predictions - predictions.mean()) ** 2))
    h2 = float(num / den) if den > 1e-12 else 0.0
    return h2, (float(den / total) if total > 1e-12 else 0.0)


def analyse(runs: pd.DataFrame, responses: List[str], factors: List[str] = None) -> Tuple[pd.DataFrame, pd.DataFrame]:
    factors = [f for f in (factors or config.FACTORS) if f in runs.columns]
    importance_rows, h_rows = [], []
    for response in responses:
        if response not in runs.columns:
            continue
        data = runs.copy()
        data["y"] = transform(data[response], response)
        data = data.dropna(subset=["y"])
        used = [f for f in factors if data[f].nunique() > 1]
        if data["unit"].nunique() < MIN_UNITS or not used:
            continue
        X, levels = _encode(data, used)
        y = data["y"].to_numpy()
        groups = data["unit"].to_numpy()
        folds = GroupKFold(n_splits=min(5, data["unit"].nunique()))

        scores, drops = [], {f: [] for f in used}
        for train, test in folds.split(X, y, groups):
            model = _model(len(used)).fit(X[train], y[train])
            scores.append(r2_score(y[test], model.predict(X[test])))
            result = permutation_importance(model, X[test], y[test], n_repeats=5, random_state=0, scoring="r2")
            for f, value in zip(used, result.importances_mean):
                drops[f].append(value)
        cv_r2 = float(np.mean(scores))
        for f in used:
            importance_rows.append(
                {"response": response, "factor": f, "importance": float(np.mean(drops[f])), "importance_sd": float(np.std(drops[f])), "cv_r2": cv_r2, "n_obs": len(data)}
            )

        model = _model(len(used)).fit(X, y)
        rng = np.random.default_rng(0)
        sample = X[rng.choice(len(X), size=min(H_SAMPLE, len(X)), replace=False)]
        for (j, a), (k, b) in combinations(enumerate(used), 2):
            h2, share = h_statistic(model, sample, j, k, len(levels[j]), len(levels[k]))
            h_rows.append({"response": response, "factor_a": a, "factor_b": b, "h2": h2, "joint_effect_share": share, "cv_r2": cv_r2})
        LOG.info("Importance of %s: CV R² = %.2f", response, cv_r2)
    return pd.DataFrame(importance_rows), pd.DataFrame(h_rows)


def plot(importance: pd.DataFrame, interactions: pd.DataFrame, output_dir: Path) -> List[Path]:
    paths = []
    if importance.empty:
        return paths
    responses = list(dict.fromkeys(importance["response"]))
    fig, axes = plotting.grid(len(responses), columns=3, size=(4.2, 2.8))
    for ax, response in zip(axes, responses):
        table = importance[importance["response"] == response].sort_values("importance")
        ax.barh(table["factor"], table["importance"], xerr=table["importance_sd"], color=plotting.CATEGORICAL[0], height=0.6, error_kw={"ecolor": plotting.MUTED, "lw": 1})
        ax.set_title(f"{response}\nCV R² = {table['cv_r2'].iloc[0]:.2f}", fontsize=9)
        ax.set_xlabel("R² lost when shuffled")
        ax.grid(axis="y", visible=False)
    paths.append(plotting.save(fig, output_dir / "importance.png"))

    if not interactions.empty:
        fig, axes = plotting.grid(len(responses), columns=3, size=(4.2, 3.6))
        for ax, response in zip(axes, responses):
            table = interactions[interactions["response"] == response]
            if table.empty:
                ax.set_visible(False)
                continue
            names = sorted(set(table["factor_a"]) | set(table["factor_b"]))
            matrix = np.full((len(names), len(names)), np.nan)
            for _, row in table.iterrows():
                i, j = names.index(row["factor_a"]), names.index(row["factor_b"])
                # Pairs with a negligible joint effect are left blank (their H² is noise)
                if row["joint_effect_share"] >= MIN_JOINT_SHARE:
                    matrix[i, j] = matrix[j, i] = row["h2"]
            plotting.heatmap(ax, matrix, names, names, vmin=0, vmax=1)
            ax.set_title(f"{response}: H² (interaction share)", fontsize=9)
        paths.append(plotting.save(fig, output_dir / "interactions_h.png"))
    return paths
