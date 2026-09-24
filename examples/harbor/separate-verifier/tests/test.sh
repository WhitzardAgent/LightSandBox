#!/usr/bin/env bash
set -euo pipefail
mkdir -p /logs/verifier
python - <<'CHECK'
from pathlib import Path
p = Path('/workspace/result.txt')
passed = p.is_file() and p.read_bytes() == b'ready\n'
Path('/logs/verifier/reward.txt').write_text('1' if passed else '0')
CHECK
