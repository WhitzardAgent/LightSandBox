# hello — minimal sanity example (planning level)

The smallest possible LightSandbox walk: print the client version, build one
`ExecSpec` through the reference contracts module, and preview the HTTP
requests for a sandbox create and an `echo hello` exec.

**Planning level, not a running sandbox.** No sandbox server ships in this
repository. Everything below is the repo's real reference CLI running in
`--dry-run` mode: it prints the exact request shape with `"executed": false`
and never contacts a daemon. The Python reference contracts live in
`client/python/src` (`main.py` inserts that path itself; nothing is installed
or fetched).

## Run

```bash
./run.sh          # or: python3 main.py
```

Requires Python >= 3.11 and the repo's reference dependency `httpx`
(already needed by `tools/lsbx.py`). No network access is used.

## Verified output (captured from a real run)

```console
$ ./run.sh
[1/4] client version (tools/lsbx.py version):
{"client_version": "0.5.0.dev0", "api_version": "v1", "runtime_implemented": false, "scope": "client-and-contract-reference"}
[2/4] ExecSpec built and validated by lightsandbox.contracts:
{"command": {"kind": "argv", "argv": ["echo", "hello"]}, "env": {}}
[3/4] dry-run sandbox create for the digest-pinned example image:
{"method": "POST", "path": "/v1/sandboxes", "body": {"image": "registry.example.invalid/demo@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "launch_mode": "image"}, "generation": null, "idempotency_key": "dry-run-key", "executed": false}
[4/4] dry-run exec request for `echo hello`:
{"method": "POST", "path": "/v1/sandboxes/sb_demo/executions", "body": {"command": {"kind": "argv", "argv": ["echo", "hello"]}, "env": {}}, "generation": 1, "idempotency_key": "dry-run-key", "executed": false}
planning-only: nothing was executed (no sandbox server ships in this repo)
```

The example image reference is deliberately unpullable
(`registry.example.invalid`, demo digest): it exercises planning and contract
validation, not image materialization.

## What a live daemon would add

With a compatible daemon reachable (configured via `LIGHTSANDBOX_ENDPOINT`),
dropping `--dry-run` would send these exact requests over HTTP — same method,
path, body, and idempotency key — and return real sandbox/exec state instead
of `executed: false` previews. That mode is not demonstrated here because the
server is not part of this repository yet.

## Self-test

`test_example.py` runs the script and asserts the planning-only shape (exit
code 0, two `executed: false` previews, request paths and bodies). It runs as
part of `./dev/check examples`.
