"""Step 3 - which combinations exist, and which blocks can be modelled as crossed.

The benchmark is not a full factorial: failures only exist in some topologies, the routing
protocols depend on the topology, some OSes do not support every NIC... A single model over
everything would attribute to one factor what belongs to another factor that always comes
with it. So:
- ``coverage.csv``: number of units for every pair of factor levels (holes are visible);
- blocks = one topology each; inside a block, a factor whose levels are fully determined by
  the factors already kept (aliased / nested) is dropped from the model, and an
  ``os x factor`` interaction is only estimated if every cell of it has data.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from itertools import combinations
from typing import Dict, List

import pandas as pd

from analysis import config

BLOCK_FACTOR = "topology"


@dataclass
class Block:
    name: str
    units: int
    factors: List[str] = field(default_factory=list)
    dropped: Dict[str, str] = field(default_factory=dict)
    interactions: List[str] = field(default_factory=list)
    incomplete_interactions: List[str] = field(default_factory=list)

    def as_row(self) -> dict:
        return {
            "block": self.name,
            "units": self.units,
            "factors": ", ".join(self.factors),
            "dropped (aliased with)": "; ".join(f"{k} ({v})" for k, v in self.dropped.items()),
            "os interactions": ", ".join(self.interactions),
            "incomplete interactions (not estimated)": ", ".join(self.incomplete_interactions),
        }


def coverage(experiments: pd.DataFrame, factors: List[str] = None) -> pd.DataFrame:
    factors = [f for f in (factors or config.FACTORS) if f in experiments.columns]
    rows = []
    for a, b in combinations(factors, 2):
        counts = experiments.groupby([a, b]).size()
        full = pd.MultiIndex.from_product([sorted(experiments[a].unique()), sorted(experiments[b].unique())], names=[a, b])
        for (level_a, level_b), n in counts.reindex(full, fill_value=0).items():
            rows.append({"factor_a": a, "level_a": level_a, "factor_b": b, "level_b": level_b, "units": int(n)})
    return pd.DataFrame(rows)


def varying(df: pd.DataFrame, factors: List[str]) -> List[str]:
    return [f for f in factors if f in df.columns and df[f].nunique() > 1]


def kept_factors(df: pd.DataFrame, candidates: List[str]) -> (List[str], Dict[str, str]):
    """Keeps factors in order, dropping those fully determined by the already kept ones."""
    kept: List[str] = []
    dropped: Dict[str, str] = {}
    for f in varying(df, candidates):
        if kept and df.groupby(kept)[f].nunique().max() <= 1:
            dropped[f] = "+".join(kept)
            continue
        # A factor that determines one already kept is nested the other way: keep the first
        determines = [k for k in kept if df.groupby(f)[k].nunique().max() <= 1 and df[k].nunique() > 1 and df.groupby(k)[f].nunique().max() <= 1]
        if determines:
            dropped[f] = "+".join(determines)
            continue
        kept.append(f)
    return kept, dropped


def complete_interaction(df: pd.DataFrame, a: str, b: str) -> bool:
    counts = df.groupby([a, b]).size()
    return len(counts) == df[a].nunique() * df[b].nunique()


def blocks(experiments: pd.DataFrame, main: str = "os", candidates: List[str] = None) -> List[Block]:
    """One block per topology (plus one over everything when there is a single topology)."""
    candidates = candidates or [main] + [f for f in config.FACTORS if f not in (main, BLOCK_FACTOR)]
    out: List[Block] = []
    for name, df in experiments.groupby(BLOCK_FACTOR):
        block = Block(name=str(name), units=len(df))
        block.factors, block.dropped = kept_factors(df, candidates)
        if main in block.factors:
            for f in block.factors:
                if f == main:
                    continue
                label = f"{main}:{f}"
                (block.interactions if complete_interaction(df, main, f) else block.incomplete_interactions).append(label)
        out.append(block)
    return out
