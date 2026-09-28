#!/usr/bin/env bash
# Runs the hello planning example against the repo's reference tooling.
# Offline: prints client version, one validated ExecSpec, and two dry-run
# request previews. Never talks to a daemon.
set -euo pipefail
exec python3 "$(dirname "$0")/main.py" "$@"
