#!/usr/bin/env bash
set -euo pipefail
mkdir -p /logs/verifier
python - <<'CHECK'
from pathlib import Path
import urllib.request
paths = ['/workspace/result.txt', '/shared/result.txt', '/workspace/fetched.txt']
passed = all(Path(p).is_file() and Path(p).read_bytes() == b'ready\n' for p in paths)
try:
    with urllib.request.urlopen('http://helper:8080/result.txt', timeout=10) as r:
        passed = passed and r.read() == b'ready\n'
except Exception:
    passed = False
Path('/logs/verifier/reward.txt').write_text('1' if passed else '0')
CHECK
