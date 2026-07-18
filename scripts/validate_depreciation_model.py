#!/usr/bin/env python3
"""Script wrapper for depreciation model validation."""

from __future__ import annotations

from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from aircost.cli.validate_depreciation_model import main


if __name__ == "__main__":
    raise SystemExit(main())
