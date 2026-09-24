#!/usr/bin/env python3
"""Observe a host without changing it. Does NOT prove sandbox support."""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import sys


def read_small(name: str, limit: int=8192):
    try:
        with open(name,'r',encoding='utf-8',errors='replace') as f:return f.read(limit)
    except OSError:return None


def observe() -> dict:
    status=read_small('/proc/self/status') or ''
    selected={}
    for line in status.splitlines():
        key,_,value=line.partition(':')
        if key in {'CapEff','NoNewPrivs','Seccomp','Seccomp_filters'}:selected[key]=value.strip()
    names=['runc','crun','ip','nsenter','buildctl','buildkitd','skopeo','umoci','fuse-overlayfs','slirp4netns','git','cargo']
    return {'schema_version':'2.0','observed_at':datetime.now(timezone.utc).isoformat(),
            'assessment':'NOT_PROBED','active_probe_performed':False,'operations':'read_only_no_network',
            'platform':{'system':platform.system(),'release':platform.release(),'machine':platform.machine(),'python':platform.python_version()},
            'effective_uid':os.geteuid() if hasattr(os,'geteuid') else None,
            'kernel_observations':{'cgroup_controllers':read_small('/sys/fs/cgroup/cgroup.controllers'),
                                   'userns_sysctl':read_small('/proc/sys/kernel/unprivileged_userns_clone'),
                                   'max_user_namespaces':read_small('/proc/sys/user/max_user_namespaces'),
                                   'process_security':selected},
            'tools_found':{name:bool(shutil.which(name)) for name in names},
            'requirements':{'kvm':'NOT_REQUIRED','kernel_replacement':'NOT_REQUIRED'},
            'warning':'Tool presence, UID and readable kernel files do not prove namespaces, quotas, networking or LSM permissions. Run T03 active tests only on an authorized worker.'}


def main() -> int:
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path);args=p.parse_args()
    text=json.dumps(observe(),ensure_ascii=False,indent=2)+'\n'
    if args.output:
        try:
            with args.output.open('x',encoding='utf-8') as f:f.write(text)
        except OSError as exc:print(f'cannot write report (no overwrite): {exc}',file=sys.stderr);return 1
    else:print(text,end='')
    return 0
if __name__=='__main__':raise SystemExit(main())
