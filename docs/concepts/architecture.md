# Architecture

LightSandbox is a capability-adaptive sandbox runtime: one contract, one planner, multiple backends. The runtime core is deliberately small and free of evaluation logic; everything about agent benchmarking lives in adapters outside the core.

## Components

```
        ┌──────────────────────────────────────┐
        │  Native API v1 (CLI · Python SDK · HTTP) │
        └──────────────────┬───────────────────┘
                           │  requirements + measured host capabilities
                    Capability Planner
                  requirement-driven selection,
                  no silent downgrade
                           │
            ┌──────────────┴──────────────┐
            ▼                             ▼
    Native backend                  Kernel backend
    rootless runc                   UML guest kernel
    InstanceSupervisor              InstanceSupervisor
    (runc · PTY · processes)        (UML process · serial channel)
```

## Crate map (`crates/`)

| Crate | Role | Contents |
|---|---|---|
| `ls-core` | Domain model, pure logic | Contract shapes (`contracts/`), host admission (`admission/`), and domain modules: backend planning (`backend.rs`), executions, lifecycle, operations, idempotency, resources, ids, errors. No I/O, no process control — decisions live here so they are unit-testable |
| `ls-api` | Wire vocabulary | Native v1 request/response types, lifecycle types, command framing, stream framing. Vocabulary only — **there is no HTTP server in this crate or anywhere in the repo** |
| `ls-host` | Host-side runtime | Rootfs materialization, image handling, exec supervision, resource guardrails, stream plumbing, and the runtime layer (manager, journal, cleanup, backend drivers under `runtime/native/`) |
| `ls-guest` | Guest-side agent | Builds `lsguest-init`, the guest PID 1 helper — the **only Rust binary** this repo produces. Statically linked (musl) so it can be PID 1 in arbitrary user images. It reaps zombies, forwards signals, and writes atomic exit receipts; on the Kernel backend it grows into the guest control agent answering over the guest channel |

The Python reference client (`client/python/`, console script `lsbx`) implements the same v1 contract for offline planning and HTTP submission.

## The supervisor: one per sandbox

Each sandbox instance is owned by exactly one lightweight **InstanceSupervisor** — never a shared daemon pool, never detached payloads reparented to PID 1 of the host. The supervisor holds the authoritative exit state, drives exec-attach, and guarantees cleanup.

Per backend:

- **Native**: supervises the runc container, its processes, and (planned) PTY plumbing.
- **Kernel**: supervises the UML process and speaks to the guest agent over a serial framed channel (the guest console is not used for control; early experiments showed userspace console writes can be silently dropped on nested hosts).

The backend-neutral supervisor interface is written in sandbox terms — create, exec, signal, exit receipt, destroy. It deliberately excludes driver-specific machinery: no runc state root, no OCI bundle paths, no pid-namespace file descriptors cross the interface. That is what keeps the API and SDK identical across backends.

## Design invariants

These rules bind everything the runtime core does:

1. **No Trial/Agent/Verifier/Reward in the runtime core.** Those are benchmark-framework concepts; they live in Harbor or other adapters, which consume sandbox/exec/file primitives like any other client.
2. **One contract.** The native API, CLI, and Python SDK share `contracts/v1/`; compatibility layers (OpenSandbox-style API, Harbor provider) adapt *into* it rather than forking it.
3. **Per-sandbox supervisor first.** Lifecycle correctness comes before density optimizations.
4. **No implicit replay.** If an execution result is lost, the exit record is `unknown` and the client decides; the runtime never silently re-runs the payload.
5. **API close ≠ destroy.** Closing a client connection never tears down guest resources.
6. **Launch mode and security policy are independent dimensions** — `image` vs `idle-agent` launch says nothing about isolation level, and vice versa.

## Failure model

Sandbox, operation, and execution each have an explicit state machine (`contracts/v1/states.json`) with a terminal `unknown` where reality was unobservable. Late authoritative evidence can move `unknown` to a real outcome, with an audit event; it never mutates silently. Cleanup failures route sandboxes to `cleanup-required` instead of pretending deletion succeeded.

Next: [backends](backends.md) for what each driver offers, [capabilities](capabilities.md) for how hosts are measured, [design trade-offs](design-tradeoffs.md) for why it is shaped this way.
