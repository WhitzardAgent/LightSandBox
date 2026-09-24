#!/usr/bin/env python3
"""Offline CLI, directly runnable from the archive; no installation required."""
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'seed/python/src'))
from lightsandbox_seed.__main__ import main
if __name__=='__main__':raise SystemExit(main())
