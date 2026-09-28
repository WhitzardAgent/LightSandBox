# Your first sandbox

!!! warning "Status: runtime-gated"
    This flow is **not yet runnable end-to-end**. The daemon and backend wiring are in development; nothing in this repository starts a server. This page explains the flow as it is frozen in `contracts/v1/`, using the contract's own vocabulary, so you can evaluate the design and write client code against it. No commands on this page are claimed to work today.

## The flow, once the runtime ships

```
create (from OCI image)  →  exec  →  read exit record  →  delete
```

Four operations, all expressed in the native v1 contract shared by the CLI, the Python SDK, and the HTTP API.

### 1. Create from an image

A sandbox is created from an **OCI image reference** (prebuilt images only today; building from a Dockerfile is planned). The create call is asynchronous: it returns an **operation**, and the sandbox moves through the state machine in `contracts/v1/states.json` — `requested → preparing → created → running`.

What the runtime does before `created`:

1. **Resolve requirements.** The spec's `requires` block (hard preconditions such as `multi_uid`, `hard_memory_limit`, `private_proc`) is checked against the host's measured capabilities. An unmet precondition rejects the create with `HOST_CAPABILITY_UNAVAILABLE` (HTTP 412) — before any resource is allocated, never by silently downgrading.
2. **Plan a backend.** The capability planner picks Native or Kernel per the `concepts/backends.md`.
3. **Materialize the rootfs.** The OCI image is unpacked into a sandbox-owned root filesystem (full copy on portable-rootless hosts).

### 2. Reference it: SandboxRef and generation

Every sandbox is addressed by a **`SandboxRef`** = `sandbox_id` + **`generation`**. The generation increments if a sandbox is destroyed and its identity rebuilt; a client holding an old generation is rejected instead of operating on the wrong sandbox. There is no implicit replay of a lost result onto a new instance.

### 3. Exec

An **`ExecSpec`** describes one execution:

- `command` — either `{kind: argv, argv: [...]}` (explicit vector, no shell expansion) or `{kind: shell, executable, script}`.
- Optional `cwd` (absolute), `env` (map), `timeout_ms`, `user`.

The exec call also returns an operation. Because execution results can be lost (crashes, disconnects), completion is observed by **polling the operation** (`op get`, or `op wait` with a timeout), never by assuming a closed connection means failure. Operations have their own lifecycle: `pending → running → succeeded | failed | unknown`.

Mutating calls carry an **idempotency key**: retrying with the same key after a network failure returns the original outcome instead of a duplicate sandbox or execution.

### 4. Read the exit record

A finished execution ends with an **`ExitRecord`** — a closed union with exactly three kinds:

| Kind | Payload | Meaning |
|---|---|---|
| `exited` | `exit_code` (0–255) | The payload exited normally |
| `signaled` | `signal` (1–127) | The payload was killed by a signal |
| `unknown` | — | The result was lost; **never assume zero** |

`unknown` exists because believability beats convenience: when the authoritative exit receipt is gone, the contract says so instead of inventing success.

### 5. Delete

Delete (`sandbox → deleting → deleted`) reaps the rootfs, supervisor, and any guest resources. Closing the API connection is **not** delete: client disconnects never destroy guest resources — an orphan sandbox stays put until explicitly deleted or reaped by policy.

## What stands between this page and reality

| Piece | State |
|---|---|
| Contract (schemas, states, resolution rules) | Frozen in `contracts/v1/`, fixture-tested |
| Reference client (CLI + SDK) | Implemented and tested against the contract |
| Native backend primitives (create/exec/delete/background/cancel) | Implemented, integration-tested at crate level |
| Daemon serving the HTTP API | In development |
| Backend wiring into the daemon | In development |

The [quickstart](quickstart.md) shows the parts you can already exercise offline: planning, contract validation, and request dry-runs.
