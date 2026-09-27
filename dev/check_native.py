#!/usr/bin/env python3
"""Run native client/contract reference tests; no container runtime is exercised."""
from pathlib import Path
import os,shutil,sys,unittest
# Round-04 §4: identical invocation law as dev/check — re-exec under a modern
# interpreter when launched by an old bare `python3`; fail loudly if none.
if sys.version_info<(3,11):
    for _c in ('python3.12','python3.11'):
        _f=shutil.which(_c)
        if _f and Path(_f).resolve()!=Path(sys.executable).resolve():
            os.execv(_f,[_f,str(Path(__file__).resolve()),*sys.argv[1:]])
    print('check_native requires Python >= 3.11 (none of python3.12/python3.11 on PATH)',file=sys.stderr)
    raise SystemExit(4)
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'client/python/src'))
if __name__=='__main__':
    suite=unittest.defaultTestLoader.discover(str(ROOT/'tests/native'))
    if suite.countTestCases()==0:raise SystemExit('zero tests collected')
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
