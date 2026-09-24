#!/usr/bin/env bash
set -euo pipefail
mkdir -p /workspace /shared
printf 'ready\n' > /workspace/result.txt
printf 'ready\n' > /shared/result.txt
python - <<'FETCH'
from pathlib import Path
import urllib.request
with urllib.request.urlopen('http://helper:8080/result.txt', timeout=10) as r:
    Path('/workspace/fetched.txt').write_bytes(r.read())
FETCH
