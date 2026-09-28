# Python SDK reference

Status: **v1 reference client — pre-release; server not bundled.** This is the
executable reference implementation of the planned v1 HTTP protocol
(`client/python/src/lightsandbox`). It proves client behavior against mocked
transports; it does not prove isolation, runtime compatibility, or command
execution, and it performs real HTTP against a compatible external daemon
when used without a mock. It is not published to a package index yet; in this
repository you import it by placing `client/python/src` on `sys.path`
(the `tools/lsbx.py` shim shows how). The only runtime dependency is `httpx`.

## Clients

```python
from lightsandbox import Client, AsyncClient

with Client('unix:///run/lightsandbox/api.sock', token='...') as c:
    ...

async with AsyncClient('https://sandbox.example.com') as c:
    ...
```

`Client` and `AsyncClient` take `endpoint`, optional `token`, and optional
`transport` (an httpx transport, used by tests). Both expose the same four
resource groups; the async versions are awaitable and
`operations.wait` polls with `asyncio.sleep`. The constructor does **not**
read environment variables — endpoints and tokens are explicit arguments
(the CLI layer is what maps `$LIGHTSANDBOX_ENDPOINT` / `$LIGHTSANDBOX_TOKEN`
onto them).

## Resource groups

| Group | Methods |
|---|---|
| `client.sandboxes` | `create(spec: dict, *, idempotency_key)`, `get(ref)`, `delete(ref, *, idempotency_key)` |
| `client.executions` | `start(ref, spec: ExecSpec, *, idempotency_key)`, `get(execution_id)`, `cancel(execution_id, *, scope, idempotency_key)` with `scope` one of `process`, `tree`, `sandbox` |
| `client.operations` | `get(operation_id)`, `wait(operation_id, *, timeout_ms=30000, poll_ms=100)` |
| `client.environments` | `prepare(plan: dict, *, idempotency_key)`, `create(template_id, *, idempotency_key)`, `get(environment_id, *, generation)`, `delete(environment_id, *, generation, *, idempotency_key)` |

References are generation-carrying: `SandboxRef(sandbox_id, generation)` —
an opaque identifier plus a positive integer generation, serialized as the
`X-LS-Generation` header on addressed requests. Mutating methods demand an
explicit `idempotency_key` (≤ 256 characters, no CR/LF); read methods never
carry one.

## Endpoint and transport rules

* `unix:///absolute/path` — local UNIX socket (the default).
* `https://host[:port]` — TLS origin; no path, query, fragment, or userinfo.
* `http://...` — **cleartext is disabled for remote hosts**: only
  `localhost`, `127.0.0.1`, and `::1` are accepted; anything else raises
  `ValueError` at client construction.
* Fixed client policy: 30-second request timeout, `trust_env=False` (proxy
  environment variables are ignored), redirects are not followed, and the
  token is sent as an `Authorization: Bearer` header (CR/LF in a token is
  rejected).

## Errors

All failures derive from `lightsandbox.ApiError` carrying `code`, a message,
and optionally `request_id` / `status_code`:

| Exception | Code | Raised when |
|---|---|---|
| `AmbiguousRequest` | `REQUEST_OUTCOME_UNKNOWN` | A **non-GET** response was lost in transit. Carries `idempotency_key`, `method`, `path`. |
| `WaitTimeout` | `WAIT_TIMEOUT` | The wait deadline elapsed. |
| `OperationUnknown` | `OPERATION_AMBIGUOUS` | An operation reported status `unknown`. |
| `ApiError` | server code, `TRANSPORT_ERROR` (GETs), `PROTOCOL_ERROR` (bad response shape), `HTTP_ERROR`/`OPERATION_FAILED` (code-less fallbacks) | Everything else. |

Three rules to design around:

* **No implicit retry, ever** — not even with an idempotency key. When a
  non-GET outcome is unknown, `AmbiguousRequest` is raised and the original
  request must be reconciled (`operations.get`, or re-`get` the addressed
  resource) before any resubmission. See
  [errors.md](errors.md) for the full code table.
* **`operations.wait` is a client-side poll loop**, not a server wait. It
  re-issues `GET /v1/operations/<id>` every `poll_ms` until the operation
  leaves `pending`/`running` or the deadline passes. `timeout_ms` caps the
  *total wait*, including each poll's own request timeout.
* **`WaitTimeout` never cancels anything.** A timed-out wait means "keep
  reconciling later": the operation may still be running server-side, and
  `OperationUnknown` after a wait means its outcome requires server-side
  reconciliation — never assume success, never assume failure.

## Contracts module

`lightsandbox.contracts` holds the pure validation layer the CLI and SDK
serialize: `SandboxRef`, `ExecSpec` (exactly one command form — argv or
explicit shell; guest-absolute cwd/shell paths; positive `timeout_ms`),
`ExitStatus` (`exited`/`signaled`/`unknown`; `normalized_code()` raises on
`unknown` instead of ever fabricating a zero), `resolve_launch` (OCI image
config to native argv/cwd/user/env for `image` and `idle-agent` modes), and
`validate_environment` (strict offline environment planning subset — unknown
fields rejected, not discarded). Validation failures raise `ContractError`,
a `ValueError`.
