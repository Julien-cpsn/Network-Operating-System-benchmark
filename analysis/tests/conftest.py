from pathlib import Path

import pytest

from analysis.tests import synthetic


@pytest.fixture(scope="session")
def synthetic_tree(tmp_path_factory) -> dict:
    root: Path = tmp_path_factory.mktemp("synthetic")
    return synthetic.build(root, runs=3)
