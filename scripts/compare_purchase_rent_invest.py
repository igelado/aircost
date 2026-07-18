#!/usr/bin/env python3
"""Script wrapper for purchase vs rent-and-invest comparison."""

from pathlib import Path
import sys

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from aircost.cli.compare_purchase_rent_invest import main


if __name__ == "__main__":
    raise SystemExit(main())
