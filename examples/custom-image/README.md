# custom-image — prebuilt OCI image, pinned by digest (planning level)

Shows the only custom-image path that exists today: you bring an **already
built** OCI image; LightSandbox addresses it by content digest. A checked-in
minimal OCI image-layout fixture lets the whole flow run offline with real
digest verification.

**Prebuilt OCI image (today's path) vs Dockerfile build (planned, not
available).** This example covers only the prebuilt side. "Hand LightSandbox
a Dockerfile and let it build" is planned functionality (an isolated BuildKit
worker produces the OCI image before the runtime consumes it) and has no
implementation here — nothing in this example builds or pulls an image, and
no Docker/BuildKit daemon is ever contacted.

## The fixture

`image/` is a complete, tiny OCI image layout (per the OCI Image
Specification):

- `oci-layout` — layout marker, `imageLayoutVersion: 1.0.0`
- `index.json` — one manifest descriptor (digest + size)
- `blobs/sha256/<manifest>` — the image manifest (schemaVersion 2)
- `blobs/sha256/<config>` — the image config (`os=linux`, `arch=amd64`, env,
  working dir, `rootfs.diff_ids`)
- `blobs/sha256/<layer>` — one 10240-byte **empty tar** layer (all zero
  blocks): the smallest valid rootfs layer there is

Every blob filename is the sha256 of its bytes; the fixture is therefore
self-verifying.

## Run

```bash
python3 plan_image.py
```

Requires Python >= 3.11 and the repo's reference dependency `httpx`. Fully
offline: verification is recomputed sha256 over checked-in bytes.

## What the script does

1. Checks `oci-layout`, reads `index.json`, then walks
   index -> manifest -> config -> layers, **recomputing every digest and
   size** and refusing drift (a byte-flipped blob fails the example).
2. For the uncompressed tar layer, verifies `rootfs.diff_ids` equals the
   layer blob digest (for compressed layers it would verify the decompressed
   digest instead — same rule, different input).
3. Prints the **pin-by-digest** reference a real request should use
   (`registry.example.invalid/custom-demo@sha256:<manifest digest>`) and shows
   the exact `sandbox create` request preview carrying it
   (`--dry-run`, `executed: false`).

Pinning the **manifest digest** — not a tag — is what makes an image
reference immutable and reproducible across machines and time; tags are
mutable aliases. The reference CLI's offline planner also flags
non-digest-pinned image references as unresolved (`image reference requires
digest resolution`), which is why every example image here is digest-pinned.

## Verified output (captured from a real run)

```console
$ python3 plan_image.py
[1/3] manifest verified: sha256:d168b240521636faa758ade32070818d6890e5d19a906c2fd907a12a0a6c490c
[2/3] config verified: sha256:a86ed5583bcbf0f5b7cabce857078648eb47f31aaf9d34bda45bfa572916f8a4 (os=linux, arch=amd64)
[3/3] layers verified: ['sha256:84ff92691f909a05b224e1c56abb4864f01b4f8e3c854e4bb4c7baf1d3f6d652']
digest chain OK -> pin-by-digest reference: registry.example.invalid/custom-demo@sha256:d168b240521636faa758ade32070818d6890e5d19a906c2fd907a12a0a6c490c
{"method": "POST", "path": "/v1/sandboxes", "body": {"image": "registry.example.invalid/custom-demo@sha256:d168b240521636faa758ade32070818d6890e5d19a906c2fd907a12a0a6c490c", "launch_mode": "image"}, "generation": null, "idempotency_key": "dry-run-key", "executed": false}
planning-only: the image is verified content, the create is a request preview
```

## What a live daemon would add

With a compatible daemon, the same digest-pinned image reference would be
materialized into a guest rootfs by the server side and used for the actual
sandbox. The digest chain you verify here is exactly what that materializer
would re-verify on its side. None of that runtime path exists in this
repository yet.

## Self-test

`test_example.py` runs the script, asserts the verified digests and the
digest-pinned request preview, and independently re-checks that every blob is
content-addressed and the layer really is an empty tar. It runs as part of
`./dev/check examples`.
