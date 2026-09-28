# Tutorial: custom images

LightSandbox has one canonical workload format: the **OCI image**. This tutorial covers what "custom image" means today and what is still on the roadmap.

## Today: prebuilt images only

Sandboxes are created from **prebuilt OCI image references**. There is no in-repo image build pipeline yet: produce the image with whatever OCI toolchain you already use (on a machine where that toolchain is available), push it to a registry your workers can reach, and reference it from LightSandbox.

Rules the planner enforces on references:

- **Pin by digest.** `registry.example/app@sha256:…` plans cleanly. A tag-only reference is not wrong, but planning flags it honestly: `image reference requires digest resolution: <service>` appears in the plan's `unresolved` list until the digest is resolved.
- **`example.invalid` references are fixtures**, not runnable assets — the repo's samples use intentionally unpullable hosts so nobody mistakes a sample for a real workload.
- **Image `USER` interacts with identity policy.** On the Native backend's single-ID hosts, an image whose `USER` is not root is rejected under the default `STRICT` policy (`POLICY_UNSUPPORTED`); the explicit `FLATTEN_ROOT` switch folds the user to root *with a disclosed degradation* — the planner never rewrites `USER` silently.

## What image contents must work with

- **Native backend**: uid 0 inside the sandbox (mapped to one unprivileged host uid); no assumption about subuid ranges; rootfs = full copy, so very large images pay copy cost per sandbox (measured: ~0.8–0.9 s for 972 MB on the development host, 2026-09).
- **Kernel backend**: arbitrary UIDs/GIDs and faithful ownership — materialization happens guest-side from the on-disk OCI config, and fidelity is evidence-backed (108/108 stat checks on the reference image).

## Roadmap: building from a Dockerfile

Building images from Dockerfiles is **planned**, via an isolated BuildKit OCI worker — the only build component on the execution path, and deliberately not a Docker daemon. The intended flow keeps one format end to end:

```
Dockerfile → BuildKit (isolated worker) → OCI image → Native rootfs materializer
                                                   → UML base-disk materializer
```

BuildKit preserves upstream build semantics; LightSandbox does not reimplement a Dockerfile parser, and per-backend materializers fork only at the image→rootfs step. Until this lands, build elsewhere and pin digests.

## The kernel image pipeline (separate concern)

The UML guest **kernel** is not an OCI image — it is a pinned, config-audited artifact whose every config symbol is classified (required / optional / decision / trap) with consequences recorded. See the kernel config artifact and module policy at `images/kernel/uml/README.md` and `images/kernel/uml/module-policy.md` in the repository (internal build artifacts, not part of the published docs).

Related: [kernel backend](kernel-backend.md) status, `reference/compatibility.md` for image-feature support per backend.
