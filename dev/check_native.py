#!/usr/bin/env python3
"""Run native client/contract reference tests; no container runtime is exercised."""
from pathlib import Path
import sys,unittest
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'client/python/src'))
if __name__=='__main__':
    suite=unittest.defaultTestLoader.discover(str(ROOT/'tests/native'))
    if suite.countTestCases()==0:raise SystemExit('zero tests collected')
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
