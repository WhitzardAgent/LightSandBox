# Quickstart

Goal: from a fresh clone to your first sandbox. Today the last mile — an actual running sandbox — is **gated on runtime work that is still in development**. This quickstart is the honest version: every step below really runs, offline, against the development baseline, and the final step shows exactly what a sandbox request will look like once a compatible daemon exists.

All commands run from the repository root. No root, no Docker, no KVM.

## 1. Prerequisites and clone

Linux x86_64, Python ≥ 3.11, and the two Python packages pinned in `dev/requirements-reference.txt`. Details: [installation](installation.md).

```bash
git clone https://github.com/WhitzardAgent/LightSandBox.git
cd LightSandBox
python3 -m pip install -r dev/requirements-reference.txt
```

## 2. Verify the pack

```bash
python3 tools/packctl.py validate
```

Expected output:

```json
{
  "command": "validate",
  "ok": true,
  "errors": [],
  "scope": "planning structure only; not runtime certification"
}
```

This checks pack structure and evidence references. It certifies nothing about runtime behavior — the scope string says so.

## 3. Client smoke test

```bash
python3 tools/lsbx.py version
```

Expected output:

```json
{
  "client_version": "0.5.0.dev0",
  "api_version": "v1",
  "runtime_implemented": false,
  "scope": "client-and-contract-reference"
}
```

`runtime_implemented: false` is printed by design: the client reports honestly that it is a contract reference, not a connected runtime.

## 4. Offline environment planning

`examples/native/environment.json` describes a one-service environment (image pinned by digest, resource requests, labels). Plan it:

```bash
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
```

Expected output (digest matches the shipped example file; shown reformatted — `--json` prints one compact line, and the `spec` block below is elided):

```json
{
  "plan_version": "v1",
  "spec": { "schema_version": "v1", "services": { "main": { "...": "..." } }, "...": "..." },
  "plan_digest": "sha256:1f1e7e666d4e9d1679c5dd18a7e89d7c1c5e1250e971b152f4be2dd6c0896572",
  "execution_ready": false,
  "unresolved": [
    "host capabilities not probed",
    "images not materialized",
    "template not prepared"
  ],
  "executed": false
}
```

How to read this:

- **`plan_digest`** is a SHA-256 over the canonicalized spec. Two engineers planning the same file get the same digest; any edit changes it. It is the identity of the plan, not an approval to run.
- **`unresolved`** lists, item by item, what stands between this plan and execution: the host has not been probed for capabilities, no image has been materialized into a rootfs, and no template has been prepared. Planning never pretends these away.
- **`execution_ready: false`** and **`executed: false`**: this command does local validation only, always.

## 5. Dry-run a sandbox exec

The client can print the exact HTTP request any operation would send:

```bash
python3 tools/lsbx.py --json exec sb_demo --generation 1 --dry-run --cwd /work -- printf '%s' 'a b'
```

Expected output:

```json
{
  "method": "POST",
  "path": "/v1/sandboxes/sb_demo/executions",
  "body": {
    "command": { "kind": "argv", "argv": ["printf", "%s", "a b"] },
    "env": {},
    "cwd": "/work"
  },
  "generation": 1,
  "idempotency_key": "dry-run-key",
  "executed": false
}
```

This is the real v1 wire shape: `SandboxRef` carries both the sandbox id and its **generation** (a stale generation is rejected, protecting against acting on a recreated sandbox), and every mutating call carries an idempotency key. Once a compatible daemon exists, the same command without `--dry-run` sends exactly this request over HTTP and returns an operation handle instead.

Secret hygiene note: `--env API_KEY=...` values are replaced by `<redacted>` in dry-run output, so previews are safe to paste into issues.

## 6. What a real run would add

A compatible daemon (in development) turns each accepted request into an **operation** you poll (`op get`, `op wait`) until it reaches `succeeded`, `failed`, or `unknown`. Executions end with an explicit exit record: `exited` (with code), `signaled` (with signal), or `unknown` — and `unknown` never reads as zero. The sandbox lifecycle that manages this is specified in `contracts/v1/states.json`; the runtime that drives it is the current engineering focus.

Deeper walkthrough of that flow: [first sandbox](first-sandbox.md).

## Where the runtime stands today

The Native backend (rootless runc) has implemented and tested create/exec/delete, background execution, and cancellation; the Kernel backend (UML) has accepted feasibility evidence and an in-progress production backend. Per-capability truth: `concepts/backends.md` and the `reference/compatibility.md`.

## Next steps

- `tutorials/run-a-sandbox.md` — the same flow, slower, with explanations
- `tutorials/backend-selection.md` — how requirements pick a backend
- `concepts/security-model.md` — what is and is not isolated today
