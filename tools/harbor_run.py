#!/usr/bin/env python3
"""Harbor launcher; dry-run works now, real execution requires the future backend."""
from pathlib import Path
import os,sys
SOURCE=Path(__file__).resolve().parents[1]/'seed/python/src'
sys.path.insert(0,str(SOURCE))
# Make the optional provider visible to the separately launched Harbor process.
os.environ['PYTHONPATH']=str(SOURCE)+(os.pathsep+os.environ['PYTHONPATH'] if os.environ.get('PYTHONPATH') else '')
from lightsandbox_harbor.launcher import main
if __name__=='__main__':raise SystemExit(main())
