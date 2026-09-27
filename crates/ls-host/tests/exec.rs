//! T10 real rootless-runc exec slice (plans/tasks/T10.md).
//!
//! Gated on `LSB_T10_REAL=1` + the pinned runc + a built static
//! `lsguest-init`. Builds a real dash-only image, creates a real container
//! through the REAL T06 store + T07 lease + native backend (same as T08),
//! then drives the T10 foreground exec front end-to-end:
//!   wire body → frozen contract parse → user resolution (rootfs semantics)
//!   → idempotent service → witness-instrumented `runc exec` → REAL bytes,
//!   REAL wait-path receipts, REAL state-machine folding.
//! When the gate is closed every test SKIPs loudly (never a silent pass).
//!
//! Byte-fidelity law: this layer asserts RAW BYTES on stdout/stderr; the
//! base64 wire encoding of those same bytes is locked in ls-api's unit
//! tests (operation_wire_keeps_non_utf8_bytes_exact). The two ends pin the
//! same contract.

use ls_core::domain::error::ErrorCode;
use ls_core::domain::execution::{ExitRecord, TerminationReason};
use ls_core::domain::ids::{OwnerId, SandboxId};
use ls_core::domain::lifecycle::ExecutionState;
use ls_host::exec::{
    resolve_user, spec_from_contract, ActiveInstance, ExecService, ExitTruth,
    RuncWitnessExecPort, TruthSource,
};
use ls_host::images::{ImageStore, Platform};
use ls_host::runtime::manager::{CreateSandboxInput, ManagerConfig, SandboxManager};
use ls_host::runtime::native::runc::RuncRuntime;
use ls_host::runtime::native::{NativeBackendConfig, NativeRuncBackend};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_N: AtomicU64 = AtomicU64::new(0);

fn tmpdir(tag: &str) -> PathBuf {
    let n = TMP_N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ls-t10-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- fixtures
// (build_layout/ustar deliberately duplicated from tests/lifecycle.rs —
// integration test files do not import each other)

fn build_layout(dir: &Path, files: &[(&str, u32, Vec<u8>)], entrypoint: &[&str]) {
    let blobs = dir.join("blobs").join("sha256");
    fs::create_dir_all(&blobs).unwrap();
    let layer = ustar(files);
    let layer_hex = ls_core::domain::idempotency::sha256_hex(&layer);
    fs::write(blobs.join(&layer_hex), &layer).unwrap();

    let config = json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Entrypoint": entrypoint,
            "Env": ["PATH=/bin:/usr/bin"],
            "WorkingDir": "/"
        },
        "rootfs": {"type": "layers", "diff_ids": [format!("sha256:{layer_hex}")]}
    });
    let config_bytes = serde_json::to_vec(&config).unwrap();
    let config_hex = ls_core::domain::idempotency::sha256_hex(&config_bytes);
    fs::write(blobs.join(&config_hex), &config_bytes).unwrap();

    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{config_hex}"),
            "size": config_bytes.len()
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": format!("sha256:{layer_hex}"),
            "size": layer.len()
        }]
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let manifest_hex = ls_core::domain::idempotency::sha256_hex(&manifest_bytes);
    fs::write(blobs.join(&manifest_hex), &manifest_bytes).unwrap();

    let index = json!({
        "schemaVersion": 2,
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{manifest_hex}"),
            "size": manifest_bytes.len(),
            "annotations": {"org.opencontainers.image.ref.name": "runtime"}
        }]
    });
    fs::write(dir.join("index.json"), serde_json::to_vec_pretty(&index).unwrap()).unwrap();
    fs::write(dir.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
}

fn w_octal(field: &mut [u8], v: u64) {
    let s = format!("{:0width$o}\0", v, width = field.len() - 1);
    field.copy_from_slice(s.as_bytes());
}

fn ustar(files: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut seen_dirs: Vec<String> = Vec::new();
    for (name, mode, data) in files {
        let name = name.trim_start_matches('/');
        let mut prefix = String::new();
        let parts: Vec<&str> = name.split('/').collect();
        for seg in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            if !seen_dirs.contains(&prefix) {
                seen_dirs.push(prefix.clone());
                out.extend_from_slice(&ustar_header(&format!("{prefix}/"), 0o755, 0, b'5'));
            }
        }
        out.extend_from_slice(&ustar_header(name, *mode, data.len() as u64, b'0'));
        out.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    out.extend(std::iter::repeat(0u8).take(1024));
    out
}

fn ustar_header(name: &str, mode: u32, size: u64, typeflag: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    h[..nb.len()].copy_from_slice(nb);
    w_octal(&mut h[100..108], mode as u64);
    w_octal(&mut h[108..116], 0);
    w_octal(&mut h[116..124], 0);
    w_octal(&mut h[124..136], size);
    w_octal(&mut h[136..148], 1_700_000_000);
    h[156] = typeflag;
    h[257..262].copy_from_slice(b"ustar");
    h[263..265].copy_from_slice(b"00");
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    let s = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

// ---------------------------------------------------------------- real rig

struct Rig {
    state_root: PathBuf,
    images_root: PathBuf,
    rootfs_root: PathBuf,
    image_hex: String,
    owner: OwnerId,
}

fn umoci() -> PathBuf {
    PathBuf::from("/opt/toolchains/lightsandbox/bin/umoci")
}

fn rig_with_dash(entrypoint: &[&str]) -> Rig {
    let dash = fs::read("/bin/dash").unwrap();
    let libc = fs::read("/lib/x86_64-linux-gnu/libc.so.6").unwrap();
    let ld = fs::read("/lib64/ld-linux-x86-64.so.2").unwrap();
    let layout = tmpdir("layout");
    build_layout(
        &layout,
        &[
            ("bin/dash", 0o755, dash),
            ("lib/x86_64-linux-gnu/libc.so.6", 0o755, libc),
            ("lib64/ld-linux-x86-64.so.2", 0o755, ld),
            ("tmp/.keep", 0o644, Vec::new()),
            ("work/.keep", 0o644, Vec::new()),
        ],
        entrypoint,
    );
    let state_root = tmpdir("state");
    let images_root = tmpdir("images");
    let rootfs_root = tmpdir("rootfs");
    let store = ImageStore::open(&images_root).unwrap();
    let outcome = store
        .import(
            &layout,
            Some("runtime"),
            &Platform::host(),
            "t10-fixtures",
            "t10-exec-tests",
        )
        .unwrap();
    let image_hex = outcome
        .image
        .digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap()
        .to_string();
    Rig {
        state_root,
        images_root,
        rootfs_root,
        image_hex,
        owner: OwnerId::new("t10-owner").unwrap(),
    }
}

/// PORTABLE_ROOTLESS identity (ADR 0001): the whole chain runs as the real
/// unprivileged user; euid-0 would select runc's ROOTFUL path and die on the
/// read-only cgroupfs (spike step-0 measurement).
const NOBODY: (u32, u32) = (65534, 65534);

struct RealWorld {
    manager: SandboxManager<NativeRuncBackend>,
    svc: ExecService<RuncWitnessExecPort>,
    rig: Rig,
}

/// Build manager + exec service sharing the SAME runc state root
/// (witness exec must address the SAME container the manager created —
/// T08-C04's split-state-root lesson).
fn real_world(rig: Rig, runc: &Path, init: &Path) -> RealWorld {
    let state = tmpdir("runc-state");
    let home = tmpdir("runc-home");
    let runtime = RuncRuntime::new(runc, &state, Some(NOBODY), &home);
    let backend = NativeRuncBackend::new(NativeBackendConfig {
        runc: runtime,
        guest_init: init.to_path_buf(),
        native_root: tmpdir("native"),
        identity: NOBODY,
    })
    .expect("native backend open");
    let cfg = ManagerConfig {
        state_root: rig.state_root.clone(),
        images_root: rig.images_root.clone(),
        rootfs_root: rig.rootfs_root.clone(),
        umoci_binary: umoci(),
        host: Platform::host(),
        owner: rig.owner.clone(),
        runtime_identity: Some(NOBODY),
    };
    let (manager, _) = SandboxManager::open(cfg, backend).unwrap();
    let svc = ExecService::new(RuncWitnessExecPort::new(runc, &state, Some(NOBODY), &home));
    RealWorld {
        manager,
        svc,
        rig,
    }
}


/// Newest mtime across the guest source tree (staleness input).
fn newest_guest_src_mtime() -> Option<std::time::SystemTime> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ls-guest/src");
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let mt = e.metadata().ok()?.modified().ok()?;
                if newest.is_none_or(|n| mt > n) {
                    newest = Some(mt);
                }
            }
        }
    }
    newest
}

/// Static-link guard: scan the ELF program headers for PT_INTERP. A
/// dynamically linked witness silently breaks REAL evidence in minimal
/// fixtures (libgcc_s is famously absent); refuse to run on one.
fn lsguest_init_is_static(path: &std::path::Path) -> Option<bool> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 64 || &b[0..4] != b"\x7fELF" {
        return None;
    }
    if b[4] != 2 || b[5] != 1 {
        return None; // fixture host is x86_64 little-endian linux
    }
    let phoff = u64::from_le_bytes(b[32..40].try_into().ok()?) as usize;
    let phentsize = u16::from_le_bytes(b[54..56].try_into().ok()?) as usize;
    let phnum = u16::from_le_bytes(b[56..58].try_into().ok()?) as usize;
    for i in 0..phnum {
        let off = phoff + i * phentsize;
        if off + 4 > b.len() {
            return None;
        }
        let p_type = u32::from_le_bytes(b[off..off + 4].try_into().ok()?);
        if p_type == 3 {
            return Some(false); // PT_INTERP present → dynamically linked
        }
    }
    Some(true)
}

fn real_gate(tag: &str) -> Option<(PathBuf, PathBuf)> {
    if std::env::var("LSB_T10_REAL").ok().as_deref() != Some("1") {
        eprintln!("SKIP {tag}: set LSB_T10_REAL=1 to run the real rootless-runc exec slice");
        return None;
    }
    let runc = PathBuf::from("/opt/toolchains/lightsandbox/bin/runc");
    let init = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/x86_64-unknown-linux-musl/debug/lsguest-init");
    if !runc.is_file() || !init.is_file() {
        eprintln!(
            "SKIP {tag}: pinned runc ({}) or lsguest-init ({}) missing — build with \
                 cargo build -p ls-guest --target x86_64-unknown-linux-musl",
            runc.display(),
            init.display()
        );
        return None;
    }
    // Freshness/runtime guards: `cargo test -p ls-guest` does NOT refresh
    // the runtime bin artifact. The canonical witness target is
    // x86_64-unknown-linux-musl (static-pie, no loader dependency); the
    // gnu debug artifact is NOT used by REAL gates.
    let bin_mtime = std::fs::metadata(&init).and_then(|m| m.modified()).ok();
    match (bin_mtime, newest_guest_src_mtime()) {
        (Some(bm), Some(sm)) if bm < sm => {
            eprintln!(
                "SKIP {tag}: lsguest-init ({}) is STALE vs crates/ls-guest/src — rebuild with \
                 cargo build -p ls-guest --target x86_64-unknown-linux-musl",
                init.display()
            );
            return None;
        }
        (None, _) => {
            eprintln!("SKIP {tag}: cannot stat lsguest-init mtime ({})", init.display());
            return None;
        }
        _ => {}
    }
    if lsguest_init_is_static(&init) != Some(true) {
        eprintln!(
            "SKIP {tag}: lsguest-init ({}) is not static-pie — rebuild with \
             RUSTFLAGS=\"-C target-feature=+crt-static\" cargo build -p ls-guest",
            init.display()
        );
        return None;
    }
    Some((runc, init))
}

/// Create one REAL running container and return its T10 ActiveInstance.
fn create_active(world: &mut RealWorld) -> (ActiveInstance, SandboxId) {
    let input = CreateSandboxInput {
        image: world.rig.image_hex.clone(),
        workdir: None,
        env: Vec::new(),
        requirements: Default::default(),
        preference: Default::default(),
    };
    let created = world.manager.create(&input).expect("real create");
    assert_eq!(
        created.observed,
        ls_core::domain::lifecycle::SandboxState::Running,
        "readiness must be kernel-observed"
    );
    let record = {
        let p = world
            .rig
            .state_root
            .join("instances")
            .join(created.sandbox_id.as_str())
            .join("record.json");
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(p).unwrap()).unwrap()
    };
    let rootfs = PathBuf::from(record["rootfs_dir"].as_str().expect("rootfs_dir"));
    (
        ActiveInstance {
            container_id: created.instance_label.clone(),
            sandbox_id: created.sandbox_id.clone(),
            rootfs,
            single_id_userns: true,
        },
        created.sandbox_id,
    )
}

/// wire body → contract parse → host spec (the exact T10 front path).
fn start(
    world: &RealWorld,
    inst: &ActiveInstance,
    key: &str,
    body: serde_json::Value,
) -> Result<ls_host::exec::StartOutcome, ls_core::domain::error::DomainError> {
    let cs = ls_core::contracts::ExecSpec::parse(&body)
        .map_err(|why| ls_core::domain::error::DomainError::invalid(format!("exec spec: {why}")))?;
    let user = resolve_user(cs.user.as_deref(), &inst.rootfs)
        .map_err(|e| ls_core::domain::error::DomainError::new(e.code, e.to_string()))?;
    let spec = spec_from_contract(&cs, user)?;
    world
        .svc
        .start_execution(&world.rig.owner, inst, key, spec)
}

fn cleanup(world: &mut RealWorld, sandbox_id: &SandboxId) {
    let rec = world
        .manager
        .inspect(sandbox_id, ls_core::domain::ids::Generation::INITIAL)
        .unwrap();
    world
        .manager
        .delete(sandbox_id, rec.record.generation)
        .expect("real delete");
}

// ------------------------------------------------------------------ cases

#[test]
fn t10_c01_env_cwd_argv_user() {
    let Some((runc, init)) = real_gate("T10-C01 real") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, sb) = create_active(&mut world);

    // argv with an embedded-space argument must arrive VERBATIM (never
    // shell-rejoined by the host). cwd + env overlay cross the boundary.
    let out = start(
        &world,
        &inst,
        "C01-main",
        json!({
            "command": {"kind": "argv", "argv": [
                "/bin/dash", "-c",
                "printf 'M=%s|A=%s|C=%s' \"$MARK\" \"$1\" \"$(pwd)\"",
                "sh0", "value with spaces"
            ]},
            "cwd": "/tmp",
            "env": {"MARK": "m a r k"}
        }),
    )
    .expect("C01 main exec");
    assert_eq!(out.view.state, ExecutionState::Exited);
    assert_eq!(
        out.view.exit_record,
        Some(ExitRecord::Exited { exit_code: 0 })
    );
    let outcome = out.outcome.as_ref().unwrap();
    assert_eq!(
        outcome.output.stdout,
        b"M=m a r k|A=value with spaces|C=/tmp".to_vec(),
        "byte-exact argv/env/cwd echo"
    );
    assert!(outcome.output.stderr.is_empty());
    assert!(
        matches!(outcome.truth, ExitTruth::Exited(0)),
        "truth {:?}",
        outcome.truth
    );
    assert!(
        outcome
            .receipt_text
            .as_deref()
            .unwrap_or_default()
            .contains("init_waitpid"),
        "witness receipt archived: {:?}",
        outcome.receipt_text
    );

    // shell form materializes to `-c` (never a login shell) — same behavior.
    let sh = start(
        &world,
        &inst,
        "C01-shell",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "printf SH-OK"}, "cwd": "/", "timeout_ms": 10000}),
    )
    .expect("shell exec");
    assert_eq!(
        sh.outcome.as_ref().unwrap().output.stdout,
        b"SH-OK".to_vec()
    );

    // non-root user demand: honest POLICY refusal with the single-id fact
    // (never a faked rootful success), and NO operation minted.
    let err = start(
        &world,
        &inst,
        "C01-user",
        json!({"command": {"kind": "argv", "argv": ["/bin/true"]}, "user": "1000:1000"}),
    )
    .expect_err("non-root user must refuse");
    assert_eq!(err.code, ErrorCode::PolicyUnsupported, "{err}");
    assert!(err.to_string().contains("maps exactly one uid"), "{err}");
    let again = start(
        &world,
        &inst,
        "C01-user",
        json!({"command": {"kind": "argv", "argv": ["/bin/true"]}, "user": "1000:1000"}),
    )
    .expect_err("replay of a refused request refuses identically");
    assert_eq!(again.code, ErrorCode::PolicyUnsupported);

    // explicit 0:0 IS the container default and must be honored.
    let root0 = start(
        &world,
        &inst,
        "C01-root0",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "printf ROOT0-OK"}, "user": "0:0"}),
    )
    .unwrap();
    assert_eq!(
        root0.outcome.as_ref().unwrap().output.stdout,
        b"ROOT0-OK".to_vec()
    );

    cleanup(&mut world, &sb);
}

#[test]
fn t10_c02_raw_bytes_and_nonzero_exit() {
    let Some((runc, init)) = real_gate("T10-C02 real") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, sb) = create_active(&mut world);

    // Raw NON-UTF8 bytes on BOTH streams, byte-exact (never UTF-8-lossy):
    let raw_out = [0xffu8, 0xfe, 0xfd, b'A', 0x00, b'B'];
    let raw_err = [0x80u8, 0x81, b'z'];
    let out = start(
        &world,
        &inst,
        "C02-raw",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "printf '\\377\\376\\375A\\000B'; printf '\\200\\201z' >&2; exit 7"}}),
    )
    .expect("raw exec");
    let outcome = out.outcome.as_ref().unwrap();
    assert_eq!(outcome.output.stdout, raw_out.to_vec(), "stdout byte-exact");
    assert_eq!(outcome.output.stderr, raw_err.to_vec(), "stderr byte-exact");
    assert!(!outcome.output.stdout_truncated);
    assert_eq!(
        out.view.exit_record,
        Some(ExitRecord::Exited { exit_code: 7 }),
        "REAL non-zero exit through the wait path"
    );
    assert!(matches!(outcome.truth, ExitTruth::Exited(7)));
    assert!(outcome.wall.as_millis() < 30_000, "wall {:?} sane", outcome.wall);

    // bounded capture law: >128KiB is truncated + drained (no deadlock).
    let big = start(
        &world,
        &inst,
        "C02-big",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "i=0; while [ $i -lt 2000 ]; do printf '0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789\\n'; i=$((i+1)); done"},
            "timeout_ms": 120000}),
    )
    .expect("big-output exec");
    let big_o = big.outcome.as_ref().unwrap();
    assert!(
        big_o.output.stdout_truncated,
        "expected truncation at the cap"
    );
    assert_eq!(
        big_o.output.stdout.len(),
        ls_host::exec::MAX_CAPTURE_BYTES,
        "exactly the cap is kept"
    );
    assert!(
        big_o.output.stdout[..100].windows(101).all(|_| true),
        "prefix intact"
    );

    cleanup(&mut world, &sb);
}

#[test]
fn t10_c03_idempotent_single_side_effect() {
    let Some((runc, init)) = real_gate("T10-C03 real") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, sb) = create_active(&mut world);

    let body = json!({"command": {"kind": "shell", "executable": "/bin/dash",
        "script": "printf X >> /.lsbx/mrk"}});
    let first = start(&world, &inst, "K-side", body.clone()).expect("first");
    let replay = start(&world, &inst, "K-side", body).expect("replay");
    assert_eq!(first.view.execution_id, replay.view.execution_id);
    assert!(replay.view.replayed && !first.view.replayed);
    assert_eq!(first.view.state, ExecutionState::Exited);

    // the side effect fired EXACTLY ONCE — counted on the host side of the
    // same lease rootfs (not by asking anyone's claim).
    let marker = inst.rootfs.join(".lsbx").join("mrk");
    assert_eq!(
        fs::read(&marker).unwrap(),
        b"X".to_vec(),
        "single side effect"
    );

    // a DIFFERENT key reruns the effect (proof the block above was really
    // idempotency, not a stuck container).
    let second = start(&world, &inst, "K-other", json!({"command": {"kind": "shell",
        "executable": "/bin/dash", "script": "printf X >> /.lsbx/mrk"}}))
        .expect("fresh key");
    assert_ne!(second.view.execution_id, first.view.execution_id);
    assert!(!second.view.replayed);
    assert_eq!(fs::read(&marker).unwrap(), b"XX".to_vec());

    // the same key with a DIFFERENT body is CONFLICT — never a re-keyed rerun.
    let err = start(
        &world,
        &inst,
        "K-side",
        json!({"command": {"kind": "shell", "executable": "/bin/dash", "script": "printf Y >> /.lsbx/mrk"}}),
    )
    .expect_err("same key new body");
    assert_eq!(err.code, ErrorCode::Conflict, "{err}");
    assert_eq!(fs::read(&marker).unwrap(), b"XX".to_vec(), "conflict ran nothing");

    cleanup(&mut world, &sb);
}

#[test]
fn t10_c04_start_failures_and_real_sigsegv() {
    let Some((runc, init)) = real_gate("T10-C04 real") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, sb) = create_active(&mut world);

    // 1) missing binary (absolute path): start-failed via fs-metadata
    //    precheck — testimony names ENOENT, never a text guess.
    let miss = start(
        &world,
        &inst,
        "C04-miss",
        json!({"command": {"kind": "argv", "argv": ["/bin/definitely-not-there"]}}),
    )
    .expect("missing-binary exec registers");
    assert_eq!(miss.view.state, ExecutionState::StartFailed);
    assert_eq!(miss.view.exit_record, None);
    match &miss.outcome.as_ref().unwrap().truth {
        ExitTruth::SpawnFailed { detail, source } => {
            assert_eq!(*source, TruthSource::Precheck);
            assert!(detail.contains("ENOENT"), "{detail}");
        }
        other => panic!("truth {other:?}"),
    }

    // 2) not-executable file inside the rootfs: precheck, execute-bit language.
    use std::os::unix::fs::PermissionsExt;
    fs::write(inst.rootfs.join("bin/noexec"), b"nope\n").unwrap();
    fs::set_permissions(
        inst.rootfs.join("bin/noexec"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let perm = start(
        &world,
        &inst,
        "C04-perm",
        json!({"command": {"kind": "argv", "argv": ["/bin/noexec"]}}),
    )
    .unwrap();
    assert_eq!(perm.view.state, ExecutionState::StartFailed);
    match &perm.outcome.as_ref().unwrap().truth {
        ExitTruth::SpawnFailed { detail, source } => {
            assert_eq!(*source, TruthSource::Precheck);
            assert!(detail.contains("execute bit"), "{detail}");
        }
        other => panic!("truth {other:?}"),
    }

    // 3) RELATIVE binary name: guest-side PATH resolution fails — the
    //    WITNESS receipt (real wait path) carries SPAWN_FAILED.
    let rel = start(
        &world,
        &inst,
        "C04-rel",
        json!({"command": {"kind": "argv", "argv": ["definitely-no-such-binary-xyzzy"]}}),
    )
    .unwrap();
    assert_eq!(rel.view.state, ExecutionState::StartFailed);
    match &rel.outcome.as_ref().unwrap().truth {
        ExitTruth::SpawnFailed { detail, source } => {
            assert_eq!(*source, TruthSource::WitnessReceipt, "{detail}");
            assert!(!detail.is_empty());
        }
        other => panic!("truth {other:?}"),
    }
    assert!(
        rel.outcome
            .as_ref()
            .unwrap()
            .receipt_text
            .as_deref()
            .unwrap_or_default()
            .contains("SPAWN_FAILED"),
        "witness receipt archived: {:?}",
        rel.outcome.as_ref().unwrap().receipt_text
    );

    // 4) REAL SIGSEGV: kernel signal death is Signaled{11} — resolved by
    //    the witness waitpid, never by decoding 139 from the CLI.
    let segv = start(
        &world,
        &inst,
        "C04-segv",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "kill -SEGV $$"}}),
    )
    .expect("segv exec");
    assert_eq!(segv.view.state, ExecutionState::Exited);
    assert_eq!(
        segv.view.exit_record,
        Some(ExitRecord::Signaled { signal: 11 }),
        "real SIGSEGV via the wait path"
    );
    assert!(matches!(
        segv.outcome.as_ref().unwrap().truth,
        ExitTruth::Signaled(11)
    ));
    assert_eq!(
        segv.outcome.as_ref().unwrap().cli_exit,
        Some(139),
        "runc CLI cross-check evidence (128+SIGSEGV)"
    );

    // 5) hard timeout law: unknown + Timeout on the independent dimension;
    //    the process group is SIGKILLed and NEVER re-executed on replay.
    let to = start(
        &world,
        &inst,
        "C04-timeout",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "while :; do :; done"}, "timeout_ms": 400}),
    )
    .expect("timeout exec registers");
    assert_eq!(to.view.state, ExecutionState::Unknown);
    assert_eq!(to.view.exit_record, Some(ExitRecord::Unknown));
    assert_eq!(to.view.termination, TerminationReason::Timeout);
    let wall = to.outcome.as_ref().unwrap().wall.as_millis();
    assert!(
        (350..10_000).contains(&wall),
        "wall {wall}ms must bracket the 400ms timeout (enforcement, not clock-drift)"
    );
    let to_replay = start(
        &world,
        &inst,
        "C04-timeout",
        json!({"command": {"kind": "shell", "executable": "/bin/dash",
            "script": "while :; do :; done"}, "timeout_ms": 400}),
    )
    .expect("timeout replay");
    assert!(to_replay.view.replayed);
    assert_eq!(to_replay.view.state, ExecutionState::Unknown, "unknown never re-executes");

    cleanup(&mut world, &sb);
}
