# Configuration

Status: v1 reference client — pre-release. **No sandbox server ships in this
repository, so there is nothing server-side to configure yet.** This page
covers the configuration surface that exists today: the reference client/CLI
environment variables, endpoint formats and the cleartext policy, and the
resource/ budget knobs that are real in code. There is intentionally **no
configuration file** format; everything below is either an environment
variable or a value embedded in the contract.

## Environment variables

| Variable | Read by | Default | Meaning |
|---|---|---|---|
| `LIGHTSANDBOX_ENDPOINT` | the `lsbx` CLI (the SDK takes the endpoint as a constructor argument instead) | `unix:///run/lightsandbox/api.sock` | Daemon endpoint for non-dry-run commands. |
| `LIGHTSANDBOX_TOKEN` | the `lsbx` CLI when building its client | unset | Bearer token sent as `Authorization: Bearer <token>`; CR/LF characters are rejected. Keep it out of argv and logs — command lines and dry-run previews are not secret-safe channels. |

A CLI `--endpoint` flag overrides `LIGHTSANDBOX_ENDPOINT`. No other
product-level environment variables exist. (The repo's own test harness uses
`LSB_*` variables as *test gates* for authorized runtime workers; those are
development machinery, not product configuration.)

## Endpoint formats and cleartext policy

The same rules apply to the CLI and the SDK constructor:

| Format | Accepted | Notes |
|---|---|---|
| `unix:///absolute/path` | yes | Local UNIX socket; must be an absolute path with no host part, query, or fragment. |
| `https://host[:port]` | yes | TLS origin only — no path beyond `/`, no query, fragment, or userinfo. |
| `http://host` | loopback only | **Remote cleartext is disabled**: `http://` is accepted solely for `localhost`, `127.0.0.1`, or `::1`. Anything else raises an error at client construction, before any connection attempt. |
| anything else (bare host, `tcp://`, paths on `http(s)`) | no | Rejected as an invalid endpoint. |

Transport policy is fixed (not configurable): 30-second per-request timeout,
proxy environment variables ignored (`trust_env=False`), redirects never
followed.

## Resource and budget knobs that exist in code

These are the only budget knobs in the repository today. Everything else a
sandbox "config" might imply (daemon settings, cgroup switches, image cache
tuning) has no public surface yet — no server ships — and the host-side
resource module deliberately offers no tuning switches: a requested hard
limit the host cannot enforce is rejected before create with every
unsupportable field named, never silently dropped.

| Knob | Where | Value rules |
|---|---|---|
| `--wait-timeout-ms` / `timeout_ms`, `poll_ms` | `lsbx op wait`, `operations.wait` | Positive integers; defaults 30000 ms / 100 ms. Caps the total client-side wait; never cancels the operation. |
| Idempotency key | every mutating CLI/SDK call | ≤ 256 characters, no CR/LF; explicit per call or freshly random per request (`dry-run-key` in previews). |
| Plan input size | `lsbx env plan --file` | 2 MiB cap; larger inputs exit 2. |
| `resources.cpu_millis` | environment services, sandbox spec | Positive integer milli-CPU. Observation/request only. |
| `resources.memory_bytes` | environment services, sandbox spec | Positive integer bytes. Observation/request only. |
| `resources.pids` | environment services, sandbox spec | Positive integer. Observation/request only. |
| `resources.storage_bytes` | environment services, sandbox spec | Positive integer bytes. Observation/request only. |

Units are canonical throughout: integer bytes, milli-CPU, PID counts, and
millisecond timeouts. The reference arithmetic rejects zero, negatives,
NaN/Infinity, unit overflow, and sub-millisecond losses instead of clamping
them, and adapter conversions must name their unit system explicitly — "MB"
is never silently read as MiB. Remember the contract rule: **hardness comes
from `requires.*` dimensions (see [capability-matrix.md](capability-matrix.md)),
never from setting a number** — the resource fields above describe demand;
they do not enforce it.
