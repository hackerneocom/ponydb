"""Discover command modules without maintaining a second test registration list."""
import importlib
from pathlib import Path


def test_modules():
    return {
        path.stem.removeprefix('test_'): importlib.import_module(f'{__package__}.{path.stem}')
        for path in sorted(Path(__file__).parent.glob('test_*.py'))
    }
