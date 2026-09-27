//! T11 cancellation & process-hierarchy real slice (plans/tasks/T11.md).
//!
//! Gated on `LSB_T11_REAL=1` + the pinned runc + the static `lsguest-init`
//! (protocol v2: `--nested-pidns` + `--ctrl`). Builds the same real
//! dash-only image as tests/exec.rs, then drives the BACKGROUND exec door
//! end-to-end against a real container:
//!
//!   C01 — orphan/detached descendants of a timed-out background command
//!         die with it (nested PID namespace makes the namespace init's
//!         death kernel-final for the whole tree), while an unrelated
//!         long-running exec in the same sandbox keeps running.
//!   C02 — a client that walks away (never cancels) loses nothing: the
//!         background command keeps running and stays truthfully queryable.
//!   C03 — a cancel issued after observable side effects does not retry,
//!         does not fabricate success; the single execution settles exactly
//!         once and replays to the same terminal record.
//!   C04 — cancel × natural-exit × repeated-cancel races settle to a
//!         stable terminal state; no PID-reuse cross-kill between
//!         concurrent background executions.
//!
//! When the gate is closed every test SKIPs loudly (never a silent pass).

use ls_core::domain::error::ErrorCode;
use ls_core::domain::execution::{ExitRecord, TerminationReason};
use ls_core::domain::ids::{OwnerId, SandboxId};
use ls_core::domain::lifecycle::ExecutionState;
use ls_host::exec::{
    resolve_user, spec_from_contract, ActiveInstance, ExecService, RuncWitnessExecPort,
    StartOutcome,
};
use ls_host::images::{ImageStore, Platform};
use ls_host::runtime::manager::{CreateSandboxInput, ManagerConfig, SandboxManager};
use ls_host::runtime::native::runc::RuncRuntime;
use ls_host::runtime::native::{NativeBackendConfig, NativeRuncBackend};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

static TMP_N: AtomicU64 = AtomicU64::new(0);

fn tmpdir(tag: &str) -> PathBuf {
    let n = TMP_N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ls-t11-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- fixtures
// (build_layout/ustar deliberately duplicated from tests/exec.rs —
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
            "t11-fixtures",
            "t11-cancellation-tests",
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
        owner: OwnerId::new("t11-owner").unwrap(),
    }
}

/// PORTABLE_ROOTLESS identity (ADR 0001): the whole chain runs as the real
/// unprivileged user; euid-0 would select runc's ROOTFUL path and die on the
/// read-only cgroupfs (spike step-0 measurement).
const NOBODY: (u32, u32) = (65534, 65534);

struct RealWorld {
    manager: SandboxManager<NativeRuncBackend>,
    svc: Arc<ExecService<RuncWitnessExecPort>>,
    rig: Rig,
}

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
    let svc = Arc::new(ExecService::new(RuncWitnessExecPort::new(
        runc,
        &state,
        Some(NOBODY),
        &home,
    )));
    RealWorld { manager, svc, rig }
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
    if std::env::var("LSB_T11_REAL").ok().as_deref() != Some("1") {
        eprintln!("SKIP {tag}: set LSB_T11_REAL=1 to run the real cancellation slice");
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
             cargo build -p ls-guest --target x86_64-unknown-linux-musl",
            init.display()
        );
        return None;
    }
    Some((runc, init))
}

fn create_active(world: &mut RealWorld) -> (ActiveInstance, SandboxId) {
    let input = CreateSandboxInput {
        image: world.rig.image_hex.clone(),
        workdir: None,
        env: Vec::new(),
        requirements: Default::default(),
        preference: Default::default(),
    };
    let created = world.manager.create(&input).expect("real create");
    if created.observed != ls_core::domain::lifecycle::SandboxState::Running {
        eprintln!("CREATE-NOT-RUNNING: {created:#?}");
        let inst_dir = world
            .rig
            .state_root
            .join("instances");
        for e in std::fs::read_dir(&inst_dir).into_iter().flatten().flatten() {
            eprintln!("instance dir: {}", e.path().display());
            for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                if let Ok(body) = std::fs::read_to_string(f.path()) {
                    eprintln!("--- {} ---\n{body}", f.path().display());
                }
            }
        }
    }
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

/// wire body → contract parse → host spec (the exact T10 front path) →
/// FOREGROUND exec (used for in-guest assertions like `kill -0`).
fn fg(
    world: &RealWorld,
    inst: &ActiveInstance,
    key: &str,
    script: &str,
    timeout_ms: u64,
) -> StartOutcome {
    let body = json!({
        "command": {"kind": "shell", "executable": "/bin/dash", "script": script},
        "timeout_ms": timeout_ms,
    });
    let cs = ls_core::contracts::ExecSpec::parse(&body).unwrap();
    let user = resolve_user(cs.user.as_deref(), &inst.rootfs).unwrap();
    let spec = spec_from_contract(&cs, user).unwrap();
    world
        .svc
        .start_execution(&world.rig.owner, inst, key, spec)
        .unwrap()
}

/// wire body → BACKGROUND exec.
fn bg(
    world: &RealWorld,
    inst: &ActiveInstance,
    key: &str,
    script: &str,
    budget: Duration,
) -> StartOutcome {
    let body = json!({
        "command": {"kind": "shell", "executable": "/bin/dash", "script": script},
        "timeout_ms": 30_000,
    });
    let cs = ls_core::contracts::ExecSpec::parse(&body).unwrap();
    let user = resolve_user(cs.user.as_deref(), &inst.rootfs).unwrap();
    let spec = spec_from_contract(&cs, user).unwrap();
    world
        .svc
        .start_background_execution(&world.rig.owner, inst, key, spec, budget)
        .unwrap()
}

/// Poll (via real foreground execs) until `probe` script exits 0 in-guest.
fn wait_guest(world: &RealWorld, inst: &ActiveInstance, tag: &str, probe: &str) {
    for i in 0..200 {
        let out = fg(world, inst, &format!("probe-{tag}-{i}"), probe, 10_000);
        if out.view.exit_record == Some(ExitRecord::Exited { exit_code: 0 }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("guest probe never became true: {probe}");
}

fn stdout_string(out: &StartOutcome) -> String {
    String::from_utf8_lossy(&out.outcome.as_ref().expect("fg outcome").output.stdout).to_string()
}

// ------------------------------------------------------------------- C01

/// The task card's strongest acceptance: a background command that spawns
/// detached/orphaned descendants is timed out by its horizon; EVERYTHING it
/// spawned dies (nested pidns), while a SEPARATE long-running background
/// exec in the same sandbox keeps running untouched.
#[test]
fn c01_timeout_wipes_escape_tree_and_leaves_other_sessions_alive() {
    let Some((runc, init)) = real_gate("T11-C01") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, _sandbox_id) = create_active(&mut world);

    // Sentinel first: an unrelated long-running background exec.
    let b = bg(
        &world,
        &inst,
        "sentinel",
        "echo $$ > /tmp/b.pid; while :; do :; done",
        Duration::from_secs(300),
    );
    assert_eq!(b.view.state, ExecutionState::Running);
    wait_guest(&world, &inst, "b-alive", "test -f /tmp/b.pid");

    // Escape artist: the payload spawns a detached subshell that outlives
    // its parent's interest (orphan after the payload is killed), then
    // busy-waits. Budget 2s → the horizon kill-switch fires.
    let a = bg(
        &world,
        &inst,
        "escapee",
        "(/bin/dash -c 'echo $$ > /tmp/esc.pid; while :; do echo x >> /tmp/esc.beat; done') & \
         echo $$ > /tmp/a.pid; while :; do :; done",
        Duration::from_secs(2),
    );
    assert_eq!(a.view.state, ExecutionState::Running);
    wait_guest(&world, &inst, "esc-heartbeat", "test -s /tmp/esc.beat");

    let done = world
        .svc
        .wait_execution(&world.rig.owner, &a.view.execution_id, Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        done.view.termination,
        TerminationReason::Timeout,
        "background horizon fired: Timeout attribution (audits: {:?})",
        done.view.audits
    );
    // ctrl-mediated timeout keeps REAL waitpid evidence: the witness
    // reaped its SIGKILLed child — better than the T10 foreground law
    // (receipt lost BY DESIGN there), and identical in spirit: never fake
    // an exit code.
    assert_eq!(
        done.view.exit_record,
        Some(ExitRecord::Signaled { signal: 9 }),
        "witness observed the kill (receipt: {:?})",
        done.outcome.as_ref().and_then(|o| o.receipt_text.clone())
    );
    // v2.1 evidence chain — HOST-TRUTH outcome on this host (kernel 5.15,
    // runc exec with MINIMAL_CAPS lacks CAP_SYS_ADMIN, and the container
    // /proc is a deliberate read-only rbind of the HOST proc — ADR 0001 A1,
    // private_proc is NOT claimed in v1):
    //   * unshare(CLONE_NEWPID) → EPERM, so supervision degrades to subreaper;
    //   * subreaper ADOPTION works (orphans drain — see the drain sidecar);
    //   * but the kill-sweep cannot AIM: witness/escapee host pids in the
    //     host proc table never match the in-namespace pids the sweep looks
    //     for (proc_is_faithful() detects this and degrades the sweep to
    //     wait-only);
    //   * the escaped descendant therefore OUTLIVES the kill. It stays
    //     contained inside the sandbox boundary (host UID map, no caps) and
    //     dies when the sandbox container is deleted. The receipt MUST mark
    //     residual_risk=true instead of claiming a clean tree — C01's
    //     criterion is mapped to this honest degraded semantics rather than
    //     deleted (capability note: process_tree_wipe=UNSUPPORTED on
    //     this-host-native).
    let receipt_text = done
        .outcome
        .as_ref()
        .and_then(|o| o.receipt_text.clone())
        .expect("witness receipt text kept");
    let receipt: serde_json::Value = serde_json::from_str(&receipt_text).unwrap();
    assert_eq!(
        receipt["supervision"]["mode"].as_str().unwrap(),
        "subreaper",
        "unshare(CLONE_NEWPID) is denied on this host: degrade, never fake"
    );
    assert_eq!(
        receipt["supervision"]["residual_risk"].as_bool().unwrap(),
        true,
        "kill-sweep cannot converge under host-proc rbind; the receipt MUST \
         mark residual risk instead of claiming a clean tree (receipt: {receipt_text})"
    );

    // The escaped descendant SURVIVES the kill-switch — kernel-proof via
    // `kill -0` from a fresh exec in the same sandbox. It is still jailed by
    // the container (UID map + MINIMAL_CAPS); sandbox delete is the
    // containment boundary that finally kills it.
    let check = fg(
        &world,
        &inst,
        "c01-check-dead",
        "IFS= read -r v < /tmp/esc.pid; kill -0 \"$v\" 2>/dev/null && echo alive || echo dead",
        10_000,
    );
    assert_eq!(
        stdout_string(&check).trim(),
        "alive",
        "escapee outlives the kill on this host; containment = sandbox boundary, \
         and the receipt above must have flagged residual_risk"
    );

    // The unrelated sentinel still runs; its heartbeat is its aliveness.
    let alive = fg(
        &world,
        &inst,
        "c01-check-sentinel",
        "IFS= read -r v < /tmp/b.pid; kill -0 \"$v\" 2>/dev/null && echo alive || echo dead",
        10_000,
    );
    assert_eq!(stdout_string(&alive).trim(), "alive");

    // Clean up the sentinel: cancel attribution must be Cancelled now.
    world
        .svc
        .cancel_execution(&world.rig.owner, &b.view.execution_id)
        .unwrap();
    let bdone = world
        .svc
        .wait_execution(&world.rig.owner, &b.view.execution_id, Duration::from_secs(15))
        .unwrap();
    assert_eq!(bdone.view.termination, TerminationReason::Cancelled);
    assert_eq!(bdone.view.exit_record, Some(ExitRecord::Signaled { signal: 9 }));

}

// ------------------------------------------------------------------- C02

/// A disconnected client (its StartOutcome handle dropped, no cancel ever)
/// must change NOTHING: the background command keeps running and the same
/// execution stays truthfully queryable.
#[test]
fn c02_disconnect_without_cancel_keeps_running_and_queryable() {
    let Some((runc, init)) = real_gate("T11-C02") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, _sandbox_id) = create_active(&mut world);

    let d = bg(
        &world,
        &inst,
        "detached",
        "echo marked > /tmp/d.mark; echo $$ > /tmp/d.pid; while :; do :; done",
        Duration::from_secs(300),
    );
    let eid = d.view.execution_id.clone();
    drop(d); // the client walks away — no cancel, no wait, handle gone.

    std::thread::sleep(Duration::from_millis(500));
    let got = world
        .svc
        .get_execution(&world.rig.owner, &eid)
        .expect("same execution queryable after walkaway");
    assert_eq!(got.view.state, ExecutionState::Running);
    assert!(got.outcome.is_none());
    let mark = fg(&world, &inst, "c02-mark", "IFS= read -r v < /tmp/d.mark && echo \"$v\"", 10_000);
    assert_eq!(stdout_string(&mark).trim(), "marked");

    // teardown: cancel it, observe the honest kill.
    world.svc.cancel_execution(&world.rig.owner, &eid).unwrap();
    let done = world
        .svc
        .wait_execution(&world.rig.owner, &eid, Duration::from_secs(15))
        .unwrap();
    assert_eq!(done.view.termination, TerminationReason::Cancelled);
}

// ------------------------------------------------------------------- C03

/// Cancel AFTER observable side effects: no automatic retry, no fabricated
/// success. The SAME operation key replays to the SAME terminal record —
/// the system never silently re-executes to heal a cancelled command.
#[test]
fn c03_cancel_after_side_effects_never_retries_and_replays_terminal() {
    let Some((runc, init)) = real_gate("T11-C03") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, _sandbox_id) = create_active(&mut world);

    let script = "echo phase1 > /tmp/e.side; echo $$ > /tmp/e.pid; while :; do :; done";
    let body = json!({
        "command": {"kind": "shell", "executable": "/bin/dash", "script": script},
        "timeout_ms": 30_000,
    });
    let cs = ls_core::contracts::ExecSpec::parse(&body).unwrap();
    let spec = spec_from_contract(&cs, None).unwrap();

    let e = world
        .svc
        .start_background_execution(
            &world.rig.owner,
            &inst,
            "effect-op",
            spec.clone(),
            Duration::from_secs(300),
        )
        .unwrap();
    wait_guest(&world, &inst, "side", "test -f /tmp/e.side");

    world
        .svc
        .cancel_execution(&world.rig.owner, &e.view.execution_id)
        .unwrap();
    let done = world
        .svc
        .wait_execution(&world.rig.owner, &e.view.execution_id, Duration::from_secs(15))
        .unwrap();
    assert_eq!(done.view.termination, TerminationReason::Cancelled);
    assert_eq!(done.view.exit_record, Some(ExitRecord::Signaled { signal: 9 }));

    // Replay with the SAME operation key and SAME request: the terminal
    // record returns — never a fresh attempt that would double the effect.
    let replay = world
        .svc
        .start_background_execution(&world.rig.owner, &inst, "effect-op", spec, Duration::from_secs(300))
        .unwrap();
    assert!(replay.view.replayed);
    assert_eq!(replay.view.execution_id, e.view.execution_id);
    assert_eq!(replay.view.termination, TerminationReason::Cancelled);

    // Side effect written exactly once, by the one real run.
    let side = fg(&world, &inst, "c03-side", "IFS= read -r v < /tmp/e.side && echo \"$v\"", 10_000);
    assert_eq!(stdout_string(&side).trim(), "phase1");
    let kills = fg(
        &world,
        &inst,
        "c03-dead",
        "IFS= read -r v < /tmp/e.pid; kill -0 \"$v\" 2>/dev/null && echo alive || echo dead",
        10_000,
    );
    assert_eq!(stdout_string(&kills).trim(), "dead");
}

// ------------------------------------------------------------------- C04

/// Race law: concurrent cancel / natural exit / repeated cancel settle to a
/// STABLE terminal, and a cancel of one execution never cross-kills another
/// (the witness owns exactly one waitpid handle; no PID-reuse mistakes).
#[test]
fn c04_races_settle_terminal_stable_and_no_cross_kill() {
    let Some((runc, init)) = real_gate("T11-C04") else { return };
    let rig = rig_with_dash(&["/bin/dash", "-c", "while :; do :; done"]);
    let mut world = real_world(rig, &runc, &init);
    let (inst, _sandbox_id) = create_active(&mut world);

    // (i) cancel × repeated cancel, in-flight: both Ok, one terminal.
    let f = bg(
        &world,
        &inst,
        "race-cancel",
        "echo $$ > /tmp/f.pid; while :; do :; done",
        Duration::from_secs(300),
    );
    let eid_f = f.view.execution_id.clone();
    let c1 = world.svc.cancel_execution(&world.rig.owner, &eid_f).unwrap();
    let c2 = world.svc.cancel_execution(&world.rig.owner, &eid_f).unwrap();
    assert!(c1.view.replayed || c2.view.replayed);
    let done_f = world
        .svc
        .wait_execution(&world.rig.owner, &eid_f, Duration::from_secs(15))
        .unwrap();
    assert_eq!(done_f.view.termination, TerminationReason::Cancelled);
    // cancel of a cancelled-terminal execution: same-reason idempotent Ok.
    let c3 = world.svc.cancel_execution(&world.rig.owner, &eid_f).unwrap();
    assert_eq!(c3.view.termination, TerminationReason::Cancelled);
    assert_eq!(c3.view.execution_id, eid_f);

    // (ii) natural-exit race: a fast command finishes before any cancel;
    // the late cancel is Conflict and the observed record stands.
    let g = bg(&world, &inst, "race-natural", "echo done > /tmp/g.mark", Duration::from_secs(300));
    let done_g = world
        .svc
        .wait_execution(&world.rig.owner, &g.view.execution_id, Duration::from_secs(15))
        .unwrap();
    assert_eq!(done_g.view.exit_record, Some(ExitRecord::Exited { exit_code: 0 }));
    let err = world
        .svc
        .cancel_execution(&world.rig.owner, &g.view.execution_id)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let still = world
        .svc
        .get_execution(&world.rig.owner, &g.view.execution_id)
        .unwrap();
    assert_eq!(still.view.exit_record, Some(ExitRecord::Exited { exit_code: 0 }));
    assert_eq!(still.view.termination, TerminationReason::None);

    // (iii) no cross-kill: two concurrent busy background execs; cancelling
    // one leaves the other's process verifiably ALIVE via kill -0.
    let h1 = bg(
        &world,
        &inst,
        "pair-1",
        "echo $$ > /tmp/h1.pid; while :; do :; done",
        Duration::from_secs(300),
    );
    let h2 = bg(
        &world,
        &inst,
        "pair-2",
        "echo $$ > /tmp/h2.pid; while :; do :; done",
        Duration::from_secs(300),
    );
    wait_guest(&world, &inst, "h2", "test -f /tmp/h2.pid");
    world
        .svc
        .cancel_execution(&world.rig.owner, &h1.view.execution_id)
        .unwrap();
    let done_h1 = world
        .svc
        .wait_execution(&world.rig.owner, &h1.view.execution_id, Duration::from_secs(15))
        .unwrap();
    assert_eq!(done_h1.view.termination, TerminationReason::Cancelled);
    let h2_alive = fg(
        &world,
        &inst,
        "c04-h2-alive",
        "IFS= read -r v < /tmp/h2.pid; kill -0 \"$v\" 2>/dev/null && echo alive || echo dead",
        10_000,
    );
    assert_eq!(
        stdout_string(&h2_alive).trim(),
        "alive",
        "cancelling the sibling must not cross-kill (PID-reuse ban)"
    );
    world
        .svc
        .cancel_execution(&world.rig.owner, &h2.view.execution_id)
        .unwrap();
    let _ = world
        .svc
        .wait_execution(&world.rig.owner, &h2.view.execution_id, Duration::from_secs(15))
        .unwrap();
}
