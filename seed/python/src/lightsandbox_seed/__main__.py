"""Offline Harbor environment inspection. Never creates or runs a sandbox."""
import argparse
import json
from pathlib import Path
import sys
from .harbor_loader import inspect_task
from .bundle import read_regular_file


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest='command',required=True)
    inspect = subs.add_parser('inspect-harbor',help='inspect task environment only')
    inspect.add_argument('task',type=Path)
    inspect.add_argument('--force-build',action='store_true')
    probe = subs.add_parser('probe-harbor-source',help='read-only AST check of a local BaseEnvironment source')
    probe.add_argument('base_py',type=Path)
    args=parser.parse_args()
    try:
        if args.command=='inspect-harbor':
            print(inspect_task(args.task,force_build=args.force_build).to_json())
            return 0
        from lightsandbox_harbor.contract_probe import analyze_base_source
        result=analyze_base_source(read_regular_file(args.base_py,limit=2*1024*1024).decode())
        print(json.dumps(result,ensure_ascii=False,indent=2))
        return 0 if result['compatible_shape'] else 2
    except (ValueError,OSError,UnicodeError) as exc:
        print(f'ERROR: {exc}',file=sys.stderr)
        return 2

if __name__=='__main__':raise SystemExit(main())
