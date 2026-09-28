# Tutorial: run a sandbox

This tutorial walks the full sandbox flow end to end — with the honest caveat up front: the runtime server is still in development, so the "run" you complete today is the **offline development flow**: verify, plan, dry-run. Every command below is real, runs from the repository root, and was executed against this checkout.

Time: ~5 minutes. Requirements: `getting-started/installation.md` completed.

## Step 1 — Verify the checkout

```bash
python3 tools/packctl.py validate
```

```json
{
  "command": "validate",
  "ok": true,
  "errors": [],
  "scope": "planning structure only; not runtime certification"
}
```

`ok: true` means the planning pack — specs, locks, evidence references — is self-consistent.

## Step 2 — Meet the client

```bash
python3 tools/lsbx.py version
```

```json
{
  "client_version": "0.5.0.dev0",
  "api_version": "v1",
  "runtime_implemented": false,
  "scope": "client-and-contract-reference"
}
```

`lsbx` is the reference console client for the native v1 API. It does two things without any server: local planning, and printing the exact HTTP requests it *would* send (`--dry-run`).

## Step 3 — Plan an environment

Look at the example spec, `examples/native/environment.json`: one service `main`, its image pinned by sha256 digest, resource requests (1000 cpu_millis, 512 MiB, 128 pids), no networks or volumes. Plan it:

```bash
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
```

Key fields of the response (output shown reformatted; `--json` prints one compact line):

- `plan_digest: "sha256:1f1e7e66…"` — content identity of the canonicalized spec. Edit any field and the digest changes; this is how two parties agree they planned the same thing.
- `unresolved: ["host capabilities not probed", "images not materialized", "template not prepared"]` — the honest gap list between plan and execution.
- `execution_ready: false` — planning is validation, never a claim of readiness.

Try breaking it: the planner is default-deny. An image reference without a digest adds `image reference requires digest resolution: <name>` to `unresolved`, and an unknown field fails validation outright.

## Step 4 — Dry-run the sandbox lifecycle

Create:

```bash
python3 tools/lsbx.py --json sandbox create --dry-run --image example.invalid/app:latest
```

```json
{"method": "POST", "path": "/v1/sandboxes", "body": {"image": "example.invalid/app:latest", "launch_mode": "image"}, "generation": null, "idempotency_key": "dry-run-key", "executed": false}
```

(The example uses an intentionally unpullable `example.invalid` image — it is for inspecting request shapes only.)

Exec into it, as the request would look against a hypothetical sandbox `sb_demo`:

```bash
python3 tools/lsbx.py --json exec sb_demo --generation 1 --dry-run --cwd /work --env API_KEY=not-a-real-secret -- printf '%s' 'a b'
```

```json
{"method": "POST", "path": "/v1/sandboxes/sb_demo/executions", "body": {"command": {"kind": "argv", "argv": ["printf", "%s", "a b"]}, "env": {"API_KEY": "<redacted>"}, "cwd": "/work"}, "generation": 1, "idempotency_key": "dry-run-key", "executed": false}
```

Two things to notice: the env value is `<redacted>` in output (request previews are safe to paste), and the command is an explicit argv vector — no shell parses it unless you ask for one via `kind: shell`.

Inspect and delete:

```bash
python3 tools/lsbx.py --json sandbox get sb_demo --generation 1 --dry-run
python3 tools/lsbx.py --json sandbox delete sb_demo --generation 1 --dry-run
```

```json
{"method": "GET", "path": "/v1/sandboxes/sb_demo", "body": null, "generation": 1, "idempotency_key": null, "executed": false}
{"method": "DELETE", "path": "/v1/sandboxes/sb_demo", "body": null, "generation": 1, "idempotency_key": "dry-run-key", "executed": false}
```

Mutating calls carry an idempotency key (`dry-run-key` by default here; a real key is generated per call or passed with `--idempotency-key`), so a retried request cannot create twins.

## Step 5 — See what a server would add

Drop `--dry-run` and the client performs real HTTP:

```bash
python3 tools/lsbx.py --json sandbox get sb_demo --generation 1
```

```json
{"error": {"code": "TRANSPORT_ERROR", "message": "cannot reach sandbox API", "request_id": null}}
```

Exit code 11. The client tried `unix:///run/lightsandbox/api.sock` (override with `--endpoint` or `LIGHTSANDBOX_ENDPOINT`) and found no listener — the correct failure, because this package ships no server. Once the daemon lands, this same call returns the sandbox record, and long-running operations return handles you poll with `op get` / `op wait` until `succeeded`, `failed`, or `unknown`.

## Where to go next

- `getting-started/first-sandbox.md` — the contract-level walkthrough of create → exec → exit record → delete
- [Backend selection](backend-selection.md) — make the planner reject or escalate on purpose
- [Native backend](native-backend.md) — what your host must look like for the fast path
