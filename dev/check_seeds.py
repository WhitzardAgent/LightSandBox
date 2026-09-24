#!/usr/bin/env python3
"""Unprivileged seed tests. Failure/zero collection propagates to the caller."""
from pathlib import Path
import sys
import unittest
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'seed/python/src'))
if __name__=='__main__':
    suite=unittest.defaultTestLoader.discover(str(ROOT/'seed/python/tests'))
    if suite.countTestCases()==0:raise SystemExit('zero tests collected')
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
