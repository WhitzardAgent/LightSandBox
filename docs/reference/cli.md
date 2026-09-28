# CLI reference (`lsbx`)

Status: v1 reference client — pre-release. **No sandbox server ships in this
repository.** The CLI performs offline planning and dry-run request previews
today; every non-`--dry-run` command performs real HTTP to a compatible
daemon that must be provided externally, and fails with a transport error
when none is reachable.

The CLI is the `lightsandbox.cli` module. It is available as the `lsbx`
console script from the reference client package, or in this repository
without any installation:

```bash
python3 tools/lsbx.py --json version
```

## Global options

| Option | Default | Meaning |
|---|---|---|
| `--endpoint ORIGIN` | `$LIGHTSANDBOX_ENDPOINT`, else `unix:///run/lightsandbox/api.sock` | Daemon endpoint (see [configuration](configuration.md)). |
| `--json` | off | Compact single-line JSON on stdout, and JSON error objects on stderr. Without it, answers are pretty-printed and errors are plain message lines. |

Authentication: when `$LIGHTSANDBOX_TOKEN` is set, requests carry
`Authorization: Bearer <token>`. There is no interactive login.

## Exit codes

| Exit | When |
|---|---|
| 0 | Success, including dry-run previews. A dry run is *not* an execution: the printed object carries `"executed": false`. |
| 2 | Usage or client-side contract error (`INVALID_ARGUMENT`): unknown arguments, bad `--env` form, both or neither command form, oversized plan input, unreadable file. |
| 8 | `WAIT_TIMEOUT`: the `op wait` deadline elapsed. The server-side operation still exists; the timeout never cancels it. |
| 10 | `REQUEST_OUTCOME_UNKNOWN` (a non-GET response was lost) or `OPERATION_AMBIGUOUS` (operation status `unknown`). The outcome must be reconciled by querying the original request/operation before any resubmission — nothing is retried implicitly. |
| 11 | Any other API error (including `INVALID_ARGUMENT` returned by a server). |

With `--json`, errors are `{"error": {"code": ..., "message": ..., "request_id": ...}}`
on stderr (`idempotency_key` is added for exit 10 ambiguity).

## Commands

### `version`

Prints client facts; never touches the network:

```bash
python3 tools/lsbx.py --json version
# {"client_version": "0.5.0.dev0", "api_version": "v1", "runtime_implemented": false, "scope": "client-and-contract-reference"}
```

### `sandbox`

```bash
lsbx sandbox create --image REF [--launch-mode image|idle-agent] [--idempotency-key KEY] [--dry-run]
lsbx sandbox get SANDBOX_ID --generation N [--dry-run]
lsbx sandbox delete SANDBOX_ID --generation N [--idempotency-key KEY] [--dry-run]
```

* `--launch-mode` defaults to `image` (the resolved image config runs);
  `idle-agent` starts the guest idle init instead.
* `--generation` is mandatory on get/delete: stale-generation requests fail
  rather than acting on a successor sandbox.
* Without `--idempotency-key`, a real request generates a fresh random key;
  a dry run uses the deterministic placeholder `dry-run-key`.

### `exec`

```bash
lsbx exec SANDBOX_ID --generation N [--cwd /abs/path] [--timeout-ms N] [--env K=V ...] \
    [--idempotency-key KEY] [--dry-run] (--shell SCRIPT --shell-executable /abs/sh | -- ARGV...)
```

Exactly one command form is required, enforced by the contract: either raw
`ARGV` after `--`, or `--shell` script text plus a guest-absolute
`--shell-executable`. Combining them, or a relative shell executable, exits 2.
`--env` repeats as `NAME=VALUE`; a missing `=` or a duplicate name exits 2.

### `op`

```bash
lsbx op get OPERATION_ID
lsbx op wait OPERATION_ID [--wait-timeout-ms 30000]
```

Wait is client-side polling with a hard deadline (default 30000 ms). Hitting
the deadline exits 8 (`WAIT_TIMEOUT`); the operation itself is not cancelled.

### `env plan`

```bash
lsbx env plan --file ENVIRONMENT_JSON
```

Offline only: validates the environment planning subset (unknown fields are
rejected), then prints a plan object — `plan_version`, `spec`,
`plan_digest` (sha256 of the canonical spec), `execution_ready` (always
`false` today), `unresolved` (what a daemon still must settle: host
capabilities, image materialization, template preparation; images whose
reference lacks a `sha256:` digest add an explicit digest-resolution entry),
and `executed: false`. Plan input is capped at 2 MiB. This command never
contacts a daemon.

## Dry-run request previews

Every mutating command accepts `--dry-run`. The preview is the exact HTTP
request that would be sent — method, path, JSON body, generation header
value, idempotency key — with `executed: false`:

```bash
python3 tools/lsbx.py --json exec --generation 1 --dry-run sb_demo -- echo hello
# {"method": "POST", "path": "/v1/sandboxes/sb_demo/executions", "body": {"command": {"kind": "argv", "argv": ["echo", "hello"]}, "env": {}}, "generation": 1, "idempotency_key": "dry-run-key", "executed": false}
```

Two deliberate preview rules: environment variable *values* are replaced by
`"<redacted>"` (a preview must never leak a secret into a log), and GET
previews show a `null` idempotency key (reads carry no key).

