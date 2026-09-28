# python — reference client typed surface (planning level)

A walk through the reference client's typed building blocks, ending with an
offline environment plan for the checked-in `environment.json`. The guiding
goal (drive a live sandbox from Python) is reduced honestly to what exists
today — the typed client surface and the offline planner.

**Planning level, not a running sandbox.** No sandbox server ships in this
repository. The script uses the real reference contracts
(`client/python/src/lightsandbox/contracts.py`) and the real repo CLI
(`tools/lsbx.py`); every step runs offline and the plan it prints is marked
`execution_ready: false` / `executed: false` by the tool itself.

## Run

```bash
python3 example.py
```

Requires Python >= 3.11 and the repo's reference dependency `httpx` (already
needed by `tools/lsbx.py`). No network access is used.

## What each step shows

1. **Typed contract objects** — `SandboxRef('sb_demo', 1)` (generation-carrying
   reference) and an `ExecSpec` in argv form with cwd/env/timeout; validation
   happens at construction. `ExitStatus(kind='exited', exit_code=0)` shows the
   `normalized_code()` result contract (an `unknown` status raises instead of
   ever being read as zero).
2. **Launch resolution** — `resolve_launch(image)` turns an OCI-style image
   config into the native argv/cwd/user/env for `image` mode (defaults:
   cwd `/`, user `0`); `mode='idle-agent'` parks the image argv aside and
   starts the guest idle init instead.
3. **Contract boundary** — `ExecSpec` accepts exactly one command form;
   passing both `argv` and `shell` raises `ContractError`, and the example
   fails loudly if that ever changes.
4. **Environment validation** — the checked-in `environment.json` passes the
   strict offline planning subset (`validate_environment`): unknown fields and
   unsupported features are rejected, not silently dropped.
5. **Offline plan** — `tools/lsbx.py env plan` validates the same file and
   prints a deterministic `plan_digest` with the unresolved items a daemon
   would still have to settle (host capabilities, image materialization,
   template preparation).

## Verified output (captured from a real run)

```console
$ python3 example.py
[1/5] typed contract objects:
SandboxRef: {"sandbox_id": "sb_demo", "generation": 1}
ExecSpec: {"command": {"kind": "argv", "argv": ["python3", "-c", "print(2 + 2)"]}, "env": {"TZ": "UTC"}, "cwd": "/work", "timeout_ms": 30000}
ExitStatus normalized exit code: 0
[2/5] launch resolution (image mode), from an OCI-style image config:
{"argv": ["/usr/bin/python3", "--version"], "cwd": "/", "user": "0", "env": ["PYTHONUNBUFFERED=1"], "mode": "image"}
idle-agent mode argv (image argv parked aside): ["/.lightsandbox/bin/init", "--idle"]
[3/5] contract boundary: argv and shell are mutually exclusive:
rejected as expected: choose argv OR explicit shell
[4/5] environment.json validated by the offline planning subset:
services: ['main']
[5/5] offline environment plan (tools/lsbx.py env plan):
{"plan_digest": "sha256:552cc80218b49489a97d5cb482ed5b62676a1a4b276450b04e1c5190eb49e2a4", "execution_ready": false, "unresolved": ["host capabilities not probed", "images not materialized", "template not prepared"], "executed": false}
planning-only: nothing was executed; see README.md for what a live daemon would add
```

## What a live daemon would add

With a compatible daemon at `LIGHTSANDBOX_ENDPOINT`, the same `ExecSpec`
would be submitted via `client.executions.start(ref, spec, idempotency_key=...)`
instead of being printed, the environment plan would go through the
`/v1/templates:prepare` + `/v1/environments` calls to become a real
environment, and the unresolved items would be answered by host probing and
image materialization on the server side. None of that is available yet —
this example deliberately stops at validated, deterministic plan data.

## Self-test

`test_example.py` runs the script, checks exit 0 and the planning markers,
verifies output determinism (the plan digest is stable), and re-validates
`environment.json` directly. It runs as part of `./dev/check examples`.
