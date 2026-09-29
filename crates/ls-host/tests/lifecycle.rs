//! T08 lifecycle vertical slice (plans/tasks/T08.md).
//!
//! * Fake-backend tests (unprivileged, always run): strict rollback on
//!   mid-create failure (T08-C03), restart reconciliation (T08-C04 logic),
//!   delete honesty (CleanupRequired), generation fencing, exit-record
//!   mapping. These still exercise the REAL T06 store + T07 lease layers;
//!   only the kernel side is replaced by a scripted `SandboxBackend`.
//! * Real-runc tests: gated on `LSB_T08_REAL=1` plus the pinned runc and a
//!   built `lsguest-init`; they build a real image from host /bin/dash and
//!   run the full create→exec→inspect→delete line (T08-C01/C02/C04-real).
//!   When the gate is closed they SKIP loudly (never a silent pass).

use ls_core::domain::backend::{BackendCapabilities, BackendKind};
use ls_core::domain::ids::{Generation, OwnerId, SandboxId};
use ls_core::domain::lifecycle::{ExecutionState, SandboxState};
use ls_host::images::store::ImageStore;
use ls_host::images::Platform;
use ls_host::runtime::backend::{
    BackendError, BackendExecOutcome, BackendExecSpec, BackendInstance, BackendSandboxSpec,
    BackendState, ExecTermination, InitReceipt, InitReceiptTermination, SandboxBackend,
};
use ls_host::runtime::manager::{
    CreateSandboxInput, ExecRequest, ManagerConfig, ManagerErrorCode, SandboxManager,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static TMP_N: AtomicU64 = AtomicU64::new(0);

fn tmpdir(tag: &str) -> PathBuf {
    let n = TMP_N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ls-t08-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- fixtures

/// Minimal OCI image layout: one USTAR layer carrying `files`, plus a
/// config whose Entrypoint/Cmd resolution the REAL T06 conversion consumes.
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

/// Hand-rolled USTAR (FIXTURE INPUT ONLY — extraction is the pinned
/// unpacker's job, never ours).
fn ustar(files: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut seen_dirs: Vec<String> = Vec::new();
    for (name, mode, data) in files {
        let name = name.trim_start_matches('/');
        // explicit parent dir entries (typeflag '5')
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
                out.extend(std::iter::repeat(0u8).take(1024 * 0));
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
    // the checksum field itself counts as ASCII spaces in the sum (POSIX)
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    let s = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

// --------------------------------------------------------------- fake side

struct FakeKernel {
    live: Vec<String>, // created, not yet deleted
    started: Vec<String>,
    deleted: Vec<String>,
    exec_calls: usize,
    created_argv: Vec<Vec<String>>,
    fail_create: bool,
    fail_start: bool,
    fail_delete: bool,
    state: BackendState,
    exec_termination: Option<ExecTermination>,
}

impl FakeKernel {
    fn new() -> Self {
        FakeKernel {
            live: Vec::new(),
            started: Vec::new(),
            deleted: Vec::new(),
            exec_calls: 0,
            created_argv: Vec::new(),
            fail_create: false,
            fail_start: false,
            fail_delete: false,
            state: BackendState::Running,
            exec_termination: None,
        }
    }
}

#[derive(Clone)]
struct FakeBackend {
    k: Arc<Mutex<FakeKernel>>,
}

impl SandboxBackend for FakeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Native
    }
    fn implementation(&self) -> Option<String> {
        Some("fake-backend (scripted kernel) v0".to_string())
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Native,
            implementation: self.implementation(),
            dimensions: Default::default(),
            available: true,
            note: "fake backend for manager-scope tests".to_string(),
        }
    }
    fn handle(&self, label: &str) -> BackendInstance {
        BackendInstance {
            label: label.to_string(),
        }
    }
    fn create(&self, spec: &BackendSandboxSpec) -> Result<BackendInstance, BackendError> {
        let mut k = self.k.lock().unwrap();
        if k.fail_create {
            return Err(BackendError::new("fake-create", "injected create failure"));
        }
        let label = ls_host::runtime::native::instance_label(
            spec.sandbox_id.as_str(),
            spec.generation,
        );
        k.created_argv.push(spec.argv.clone());
        k.live.push(label.clone());
        Ok(BackendInstance { label })
    }
    fn start(&self, instance: &BackendInstance) -> Result<(), BackendError> {
        let mut k = self.k.lock().unwrap();
        if k.fail_start {
            return Err(BackendError::new("fake-start", "injected start failure"));
        }
        k.started.push(instance.label.clone());
        Ok(())
    }
    fn state(&self, instance: &BackendInstance) -> Result<BackendState, BackendError> {
        let k = self.k.lock().unwrap();
        if !k.live.contains(&instance.label) {
            return Ok(BackendState::NotFound);
        }
        Ok(match k.state {
            BackendState::Running => {
                if k.started.contains(&instance.label) {
                    BackendState::Running
                } else {
                    BackendState::Created
                }
            }
            other => other,
        })
    }
    fn exec(
        &self,
        _instance: &BackendInstance,
        _exec: &BackendExecSpec,
    ) -> Result<BackendExecOutcome, BackendError> {
        let mut k = self.k.lock().unwrap();
        k.exec_calls += 1;
        Ok(BackendExecOutcome {
            termination: k.exec_termination.clone().unwrap_or(ExecTermination::Exited(0)),
            stdout: "FAKE_STDOUT".to_string(),
            stderr: String::new(),
            wall_ms: 3,
            truncated: false,
        })
    }
    fn init_receipt(&self, _instance: &BackendInstance) -> Result<Option<InitReceipt>, BackendError> {
        Ok(None)
    }
    fn delete(&self, instance: &BackendInstance) -> Result<(), BackendError> {
        let mut k = self.k.lock().unwrap();
        if k.fail_delete {
            return Err(BackendError::new("fake-delete", "injected delete failure"));
        }
        k.live.retain(|l| l != &instance.label);
        k.deleted.push(instance.label.clone());
        Ok(())
    }
    fn residue_paths(&self, _instance: &BackendInstance) -> Vec<PathBuf> {
        Vec::new()
    }
    fn enumerate_live(&self) -> Vec<String> {
        self.k.lock().unwrap().live.clone()
    }
}

// --------------------------------------------------------------- rigging

struct Rig {
    state_root: PathBuf,
    images_root: PathBuf,
    rootfs_root: PathBuf,
    image_hex: String,
    owner: OwnerId,
}

fn umoci() -> PathBuf {
    std::env::var_os("LSB_UMOCI_BIN")
        .filter(|p| std::path::Path::new(p).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/toolchains/lightsandbox/bin/umoci"))
}

fn rig_with_layout(files: &[(&str, u32, Vec<u8>)], entrypoint: &[&str]) -> Rig {
    let layout = tmpdir("layout");
    build_layout(&layout, files, entrypoint);
    let state_root = tmpdir("state");
    let images_root = tmpdir("images");
    let rootfs_root = tmpdir("rootfs");
    let store = ImageStore::open(&images_root).unwrap();
    let outcome = store
        .import(
            &layout,
            Some("runtime"),
            &Platform::host(),
            "t08-fixtures",
            "t08-lifecycle-tests",
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
        owner: OwnerId::new("t08-owner").unwrap(),
    }
}

impl Rig {
    fn config(&self) -> ManagerConfig {
        ManagerConfig {
            state_root: self.state_root.clone(),
            images_root: self.images_root.clone(),
            rootfs_root: self.rootfs_root.clone(),
            umoci_binary: umoci(),
            host: Platform::host(),
            owner: self.owner.clone(),
            runtime_identity: None,
        }
    }
    fn input(&self) -> CreateSandboxInput {
        CreateSandboxInput {
            image: self.image_hex.clone(),
            workdir: None,
            env: Vec::new(),
            entrypoint_override: None,
            command_override: None,
            requirements: Default::default(),
            preference: Default::default(),
        }
    }
    fn record_json(&self, id: &SandboxId) -> serde_json::Value {
        let p = self
            .state_root
            .join("instances")
            .join(id.as_str())
            .join("record.json");
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }
    fn sandbox_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for base in [self.rootfs_root.join("sandboxes"), self.rootfs_root.clone()] {
            if let Ok(rd) = fs::read_dir(&base) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with("sbx-") {
                        out.push(e.path());
                    }
                }
            }
        }
        out
    }
}

fn default_rig() -> Rig {
    rig_with_layout(
        &[("hello.txt", 0o644, b"fixture-rootfs\n".to_vec())],
        &["/bin/true"],
    )
}

// ------------------------------------------------------- fake-backed tests

#[test]
fn create_applies_explicit_process_overrides_without_changing_image() {
    use ls_host::images::process_config::ArgvOverride;
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    let fake = FakeBackend { k: kernel.clone() };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let mut input = rig.input();
    input.entrypoint_override = Some(ArgvOverride::Clear);
    input.command_override = Some(ArgvOverride::Replace(vec![
        "/bin/sh".into(), "-c".into(), "tail -f /dev/null".into(),
    ]));
    let created = mgr.create(&input).unwrap();
    assert_eq!(kernel.lock().unwrap().created_argv, vec![vec![
        "/bin/sh", "-c", "tail -f /dev/null",
    ]]);
    assert!(mgr.delete(&created.sandbox_id, created.generation).unwrap().all_ok());
}

#[test]
fn create_persists_record_with_legal_states() {
    let rig = default_rig();
    let fake = FakeBackend {
        k: Arc::new(Mutex::new(FakeKernel::new())),
    };
    let (mut mgr, open_report) = SandboxManager::open(rig.config(), fake.clone()).unwrap();
    assert_eq!(open_report.instances_seen, 0);
    let created = mgr.create(&rig.input()).unwrap();
    assert_eq!(created.observed, SandboxState::Running);
    assert!(created.instance_label.starts_with("lsbx-"));
    let rec = rig.record_json(&created.sandbox_id);
    assert_eq!(rec["state"], json!("running"));
    assert_eq!(rec["lifetime"], json!("ACTIVE"));
    assert!(rec["revision"].as_u64().unwrap() >= 2);
    // inspect is observation-driven and matches
    let info = mgr
        .inspect(&created.sandbox_id, created.generation)
        .unwrap();
    assert_eq!(info.state, SandboxState::Running);
    assert_eq!(info.observed_backend, BackendState::Running);
}

#[test]
fn t08_c03_backend_create_failure_rolls_back_everything() {
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    kernel.lock().unwrap().fail_create = true;
    let fake = FakeBackend { k: kernel.clone() };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let err = mgr.create(&rig.input()).unwrap_err();
    assert_eq!(err.code, ManagerErrorCode::Unavailable);
    assert!(err.message.contains("injected create failure"), "{err}");
    // strictly zero residue: no lease, no view, no record, no instance
    assert!(rig.sandbox_dirs().is_empty(), "lease residue left");
    let views = rig.state_root.join("views");
    assert!(
        !views.exists() || fs::read_dir(&views).unwrap().next().is_none(),
        "layout view residue left"
    );
    let instances = rig.state_root.join("instances");
    assert!(fs::read_dir(&instances).unwrap().next().is_none());
    assert!(kernel.lock().unwrap().live.is_empty());
}

#[test]
fn t08_c03_start_failure_compensates_through_the_same_boundary() {
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    kernel.lock().unwrap().fail_start = true;
    let fake = FakeBackend { k: kernel.clone() };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let err = mgr.create(&rig.input()).unwrap_err();
    assert!(err.message.contains("injected start failure"), "{err}");
    let k = kernel.lock().unwrap();
    // the FAILED step registers no compensation: exactly the earlier
    // backend-create is rolled back (LIFO), through the same boundary
    assert_eq!(k.live.len(), 0, "container must be deleted by compensation");
    assert_eq!(k.deleted.len(), 1, "backend-create compensation only");
    drop(k);
    assert!(rig.sandbox_dirs().is_empty(), "lease residue left");
}

#[test]
fn execute_maps_termination_and_fences_generation() {
    let rig = default_rig();
    let fake = FakeBackend {
        k: Arc::new(Mutex::new(FakeKernel::new())),
    };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let created = mgr.create(&rig.input()).unwrap();
    let out = mgr
        .execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["/bin/echo".into(), "hello".into()],
                env: vec![],
                cwd: None,
                timeout: Some(Duration::from_secs(5)),
            },
        )
        .unwrap();
    assert_eq!(out.state, ExecutionState::Exited);
    assert_eq!(out.stdout, "FAKE_STDOUT");
    assert_eq!(out.termination, ls_core::domain::execution::TerminationReason::None);
    // generation fence
    let err = mgr
        .execute(
            &created.sandbox_id,
            Generation(created.generation.get() + 9),
            &ExecRequest {
                argv: vec!["/bin/true".into()],
                env: vec![],
                cwd: None,
                timeout: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ManagerErrorCode::StaleGeneration);
}

#[test]
fn execute_timeout_never_fabricates_an_exit_code() {
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    kernel.lock().unwrap().exec_termination = Some(ExecTermination::Timeout);
    let fake = FakeBackend { k: kernel };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let created = mgr.create(&rig.input()).unwrap();
    let out = mgr
        .execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["/bin/sleep".into(), "86400".into()],
                env: vec![],
                cwd: None,
                timeout: Some(Duration::from_millis(50)),
            },
        )
        .unwrap();
    assert_eq!(out.state, ExecutionState::Unknown);
    assert_eq!(
        out.exit_record,
        Some(ls_core::domain::execution::ExitRecord::Unknown)
    );
    assert_eq!(
        out.termination,
        ls_core::domain::execution::TerminationReason::Timeout
    );
}

#[test]
fn delete_archives_receipt_retires_and_refuses_reentry() {
    let rig = default_rig();
    let fake = FakeBackend {
        k: Arc::new(Mutex::new(FakeKernel::new())),
    };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let created = mgr.create(&rig.input()).unwrap();
    // plant a REAL guest-init receipt inside the lease rootfs (the
    // host/guest wire artifact delete must archive)
    let rec = rig.record_json(&created.sandbox_id);
    let rootfs = PathBuf::from(rec["rootfs_dir"].as_str().unwrap());
    fs::create_dir_all(rootfs.join(".lsbx")).unwrap();
    fs::write(
        rootfs.join(".lsbx").join("receipt.json"),
        "{\"version\":1,\"sandbox_id\":\"x\",\"child_pid\":42,\
         \"termination\":{\"kind\":\"EXITED\",\"exit_code\":7},\"source\":\"test\"}",
    )
    .unwrap();
    let info = mgr.delete(&created.sandbox_id, created.generation).unwrap();
    assert!(info.all_ok(), "degraded delete: {:?}", info.failed_steps());
    assert_eq!(info.target_sandbox_id, created.sandbox_id.as_str());
    assert_eq!(info.ops_terminated, None, "v1 must never claim zero inflight");
    let archived = info.archived_init_receipt.expect("receipt archived");
    assert!(archived.contains("EXITED"));
    assert!(archived.contains("\"exit_code\":7"));
    assert!(info.steps.iter().any(|s| s.name == "lease-teardown" && s.ok));
    let rec = rig.record_json(&created.sandbox_id);
    assert_eq!(rec["state"], json!("deleted"));
    assert_eq!(rec["lifetime"], json!("RETIRED"));
    assert!(!rec["cleanup_ids"].as_array().unwrap().is_empty());
    assert!(rig.sandbox_dirs().is_empty(), "lease residue after delete");
    // retired ids are never reusable: NOT_FOUND, not a phantom.
    let err = mgr.delete(&created.sandbox_id, created.generation).unwrap_err();
    assert_eq!(err.code, ManagerErrorCode::NotFound);
}

#[test]
fn degraded_delete_lands_in_cleanup_required_and_retry_converges() {
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    let fake = FakeBackend { k: kernel.clone() };
    let (mut mgr, _) = SandboxManager::open(rig.config(), fake).unwrap();
    let created = mgr.create(&rig.input()).unwrap();
    kernel.lock().unwrap().fail_delete = true;
    let info = mgr.delete(&created.sandbox_id, created.generation).unwrap();
    assert!(!info.all_ok());
    assert!(info.failed_steps().iter().any(|s| s.name == "backend-delete"));
    let rec = rig.record_json(&created.sandbox_id);
    assert_eq!(rec["state"], json!("cleanup-required"));
    assert_eq!(rec["lifetime"], json!("ACTIVE"), "not silently retired");
    // retry after the kernel fault heals: converges to deleted
    kernel.lock().unwrap().fail_delete = false;
    let info2 = mgr.delete(&created.sandbox_id, created.generation).unwrap();
    assert!(info2.all_ok(), "retry: {:?}", info2.failed_steps());
    let rec = rig.record_json(&created.sandbox_id);
    assert_eq!(rec["state"], json!("deleted"));
    assert_eq!(rec["lifetime"], json!("RETIRED"));
}

#[test]
fn t08_c04_reopen_reconciles_record_against_live_kernel() {
    let rig = default_rig();
    let kernel = Arc::new(Mutex::new(FakeKernel::new()));
    let fake = FakeBackend { k: kernel.clone() };
    let created_id;
    let generation;
    {
        let (mut mgr, _) = SandboxManager::open(rig.config(), fake.clone()).unwrap();
        let created = mgr.create(&rig.input()).unwrap();
        created_id = created.sandbox_id;
        generation = created.generation;
        // manager dropped here (simulated service restart); lease + record
        // stay on disk, kernel state stays in the scripted world.
    }
    // kernel said the instance stopped while the manager was gone
    kernel.lock().unwrap().state = BackendState::Stopped;
    let (mgr, report) = SandboxManager::open(rig.config(), fake).unwrap();
    assert_eq!(report.instances_seen, 1);
    assert!(
        report.foreign_instances.is_empty(),
        "owned instance must NOT be reported foreign: {:?}",
        report.foreign_instances
    );
    let info = mgr.inspect(&created_id, generation).unwrap();
    assert_eq!(info.state, SandboxState::Stopped);
    assert_eq!(info.observed_backend, BackendState::Stopped);
    let rec = rig.record_json(&created_id);
    let audits = rec["audits"].to_string();
    assert!(audits.contains("reconcile") || audits.contains("inspect"), "{audits}");
    // foreign leftover visibility
    kernel.lock().unwrap().live.push("lsbx-foreign-1".to_string());
    let (_mgr2, report2) = SandboxManager::open(rig.config(), FakeBackend { k: kernel }).unwrap();
    assert_eq!(report2.foreign_instances, vec!["lsbx-foreign-1".to_string()]);
}

// --------------------------------------------------------- real-runc tests


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
    if std::env::var("LSB_T08_REAL").ok().as_deref() != Some("1") {
        eprintln!("SKIP {tag}: set LSB_T08_REAL=1 to run the real rootless-runc slice");
        return None;
    }
    let runc = ls_host::runtime::native::runc::RuncRuntime::resolve_binary();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let init = target.join("x86_64-unknown-linux-musl/debug/lsguest-init");
    if !runc.is_file() || !init.is_file() {
        eprintln!(
            "SKIP {tag}: pinned runc ({}) or lsguest-init ({}) missing — build with \
                 cargo build -p ls-guest --target x86_64-unknown-linux-musl",
            runc.display(),
            init.display()
        );
        panic!("{tag}: requested real gate lacks pinned runc or guest init");
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
            panic!("{tag}: requested real gate has stale guest init");
        }
        (None, _) => {
            eprintln!("SKIP {tag}: cannot stat lsguest-init mtime ({})", init.display());
            panic!("{tag}: requested real gate cannot stat guest init");
        }
        _ => {}
    }
    if lsguest_init_is_static(&init) != Some(true) {
        eprintln!(
            "SKIP {tag}: lsguest-init ({}) is not static-pie — rebuild with \
             RUSTFLAGS=\"-C target-feature=+crt-static\" cargo build -p ls-guest",
            init.display()
        );
        panic!("{tag}: requested real gate guest init is not static");
    }
    Some((runc, init))
}

/// PORTABLE_ROOTLESS identity (ADR 0001 / spike): the whole chain —
/// lease, bundle, runc process, container uid 0 — runs as the real
/// unprivileged user 65534. euid-0 takes runc's ROOTFUL cgroupfs path and
/// dies on the read-only cgroupfs (measured on this host class).
fn runtime_identity() -> (u32, u32) {
    if std::env::var("LSB_REAL_CURRENT_USER").ok().as_deref() == Some("1") {
        unsafe extern "C" { fn geteuid() -> u32; fn getegid() -> u32; }
        return unsafe { (geteuid(), getegid()) };
    }
    (65534, 65534)
}

fn nobody_cfg(cfg: &mut ManagerConfig) {
    cfg.runtime_identity = Some(runtime_identity());
}

fn real_manager(rig: &Rig, runc: &Path, init: &Path) -> SandboxManager<ls_host::runtime::native::NativeRuncBackend> {
    use ls_host::runtime::native::runc::RuncRuntime;
    use ls_host::runtime::native::{NativeBackendConfig, NativeRuncBackend};
    let state = tmpdir("runc-state");
    let home = tmpdir("runc-home");
    let runtime = RuncRuntime::new(runc, &state, Some(runtime_identity()), &home);
    let backend = NativeRuncBackend::new(NativeBackendConfig {
        runc: runtime,
        guest_init: init.to_path_buf(),
        native_root: tmpdir("native"),
        identity: runtime_identity(),
    })
    .expect("native backend open");
    let mut cfg = rig.config();
    nobody_cfg(&mut cfg);
    let (mgr, _) = SandboxManager::open(cfg, backend).unwrap();
    mgr
}

#[test]
fn t08_c01_real_create_exec_write_delete() {
    let Some((runc, init)) = real_gate("T08-C01 real") else { return };
    let dash = fs::read("/bin/dash").unwrap();
    let libc = fs::read("/lib/x86_64-linux-gnu/libc.so.6").unwrap();
    let ld = fs::read("/lib64/ld-linux-x86-64.so.2").unwrap();
    let rig = rig_with_layout(
        &[
            ("bin/dash", 0o755, dash),
            ("lib/x86_64-linux-gnu/libc.so.6", 0o755, libc),
            ("lib64/ld-linux-x86-64.so.2", 0o755, ld),
            ("tmp/.keep", 0o644, Vec::new()),
        ],
        &["/bin/dash", "-c", "while :; do :; done"], // dash-only longevity (image has no /bin/sleep)
    );
    let mut mgr = real_manager(&rig, &runc, &init);
    let created = mgr.create(&rig.input()).expect("real create");
    assert_eq!(
        created.observed,
        SandboxState::Running,
        "readiness must be kernel-observed"
    );
    // The image carries dash ONLY (no coreutils — R4 proved `cat` is absent):
    // write + read-back with pure builtins. exit_code 0 proves the spec'd
    // cmdline really ran with our arg semantics; the RBCHECK echo crosses the
    // REAL stdout pipe back to the host, so the captured bytes are instance-
    // produced, not a fixed/global result.
    let out = mgr
        .execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec![
                    "/bin/dash".into(),
                    "-c".into(),
                    "echo T08_WRITE_OK > /tmp/proof && read back < /tmp/proof && [ \"$back\" = T08_WRITE_OK ] && echo \"RBCHECK:$back\"".into(),
                ],
                env: vec![],
                cwd: None,
                timeout: Some(Duration::from_secs(20)),
            },
        )
        .expect("real exec");
    assert_eq!(out.state, ExecutionState::Exited, "stderr: {}", out.stderr);
    assert_eq!(
        out.exit_record,
        Some(ls_core::domain::execution::ExitRecord::Exited { exit_code: 0 }),
        "write+read-back inside the guest must exit 0 (proves the spec'd cmdline RAN): {:?}",
        out.exit_record
    );
    assert_eq!(
        out.stdout.trim(),
        "RBCHECK:T08_WRITE_OK",
        "real bytes through the real pipe"
    );
    assert!(
        !out.execution_id.as_str().is_empty(),
        "a per-call generated execution id, never a fixed global result"
    );
    eprintln!(
        "T08-C01 evidence: exec_id={} wall={}ms stdout={:?} truncated={}",
        out.execution_id.as_str(),
        out.wall_ms,
        out.stdout.trim(),
        out.truncated
    );
    let info = mgr.delete(&created.sandbox_id, created.generation).unwrap();
    assert!(info.all_ok(), "delete degraded: {:?}", info.failed_steps());
    assert!(info
        .steps
        .iter()
        .all(|s| s.ok), "every cleanup step really ran");
    assert!(mgr.backend().enumerate_live().is_empty(), "live containers left");
    assert!(rig.sandbox_dirs().is_empty(), "lease residue after real delete");
}

#[test]
fn t08_c02_real_entrypoint_exit7_is_the_truth() {
    let Some((runc, init)) = real_gate("T08-C02 real") else { return };
    let dash = fs::read("/bin/dash").unwrap();
    let libc = fs::read("/lib/x86_64-linux-gnu/libc.so.6").unwrap();
    let ld = fs::read("/lib64/ld-linux-x86-64.so.2").unwrap();
    let rig = rig_with_layout(
        &[
            ("bin/dash", 0o755, dash),
            ("lib/x86_64-linux-gnu/libc.so.6", 0o755, libc),
            ("lib64/ld-linux-x86-64.so.2", 0o755, ld),
        ],
        &["/bin/dash", "-c", "exit 7"],
    );
    let mut mgr = real_manager(&rig, &runc, &init);
    let created = mgr.create(&rig.input()).expect("real create (short-lived)");
    // readiness honesty: a short-lived entrypoint may ALREADY be stopped
    assert!(
        matches!(created.observed, SandboxState::Running | SandboxState::Stopped),
        "unexpected observation {:?}",
        created.observed
    );
    let info = mgr
        .inspect(&created.sandbox_id, created.generation)
        .unwrap();
    assert_eq!(info.state, SandboxState::Stopped);
    let receipt = info.init_receipt.expect("init receipt is the exit truth");
    assert_eq!(
        receipt.termination,
        InitReceiptTermination::Exited { exit_code: 7 },
        "the REAL exit code, never a Running-forever success"
    );
    eprintln!("T08-C02 evidence: receipt={receipt:?}");
    let cleanup = mgr.delete(&created.sandbox_id, created.generation).unwrap();
    assert!(cleanup.all_ok(), "{:?}", cleanup.failed_steps());
    assert!(cleanup
        .archived_init_receipt
        .as_deref()
        .unwrap_or("")
        .contains("\"exit_code\":7"));
}

#[test]
fn t08_c04_real_reopen_sees_kernel_not_db() {
    let Some((runc, init)) = real_gate("T08-C04 real") else { return };
    let dash = fs::read("/bin/dash").unwrap();
    let libc = fs::read("/lib/x86_64-linux-gnu/libc.so.6").unwrap();
    let ld = fs::read("/lib64/ld-linux-x86-64.so.2").unwrap();
    let rig = rig_with_layout(
        &[
            ("bin/dash", 0o755, dash),
            ("lib/x86_64-linux-gnu/libc.so.6", 0o755, libc),
            ("lib64/ld-linux-x86-64.so.2", 0o755, ld),
        ],
        &["/bin/dash", "-c", "while :; do :; done"], // dash-only longevity (image has no /bin/sleep)
    );
    // runc's container registry is SCOPED BY ITS STATE ROOT: pointing a
    // "restarted" manager at FRESH roots makes the live container invisible
    // (the R4 defect). A real daemon restart keeps ALL durable roots — manager
    // state (rig), runc state, native root — and replaces ONLY the process.
    // Model exactly that: one root set, two opens.
    let runc_state = tmpdir("runc-state");
    let runc_home = tmpdir("runc-home");
    let native_root = tmpdir("native");
    let open = || {
        use ls_host::runtime::native::runc::RuncRuntime;
        use ls_host::runtime::native::{NativeBackendConfig, NativeRuncBackend};
        let runtime = RuncRuntime::new(&runc, runc_state.clone(), Some(runtime_identity()), runc_home.clone());
        let backend = NativeRuncBackend::new(NativeBackendConfig {
            runc: runtime,
            guest_init: init.clone(),
            native_root: native_root.clone(),
            identity: runtime_identity(),
        })
        .unwrap();
        let mut cfg = rig.config();
        nobody_cfg(&mut cfg);
        SandboxManager::open(cfg, backend).unwrap()
    };
    let sid;
    let gen;
    {
        let (mut mgr, _) = open();
        let created = mgr.create(&rig.input()).expect("real create");
        assert_eq!(created.observed, SandboxState::Running);
        sid = created.sandbox_id;
        gen = created.generation;
        // manager dropped: the sandbox must survive WITHOUT any daemon
    }
    // "service restart": fresh PROCESS over the SAME durable roots
    let (mgr, report) = open();
    assert_eq!(report.instances_seen, 1);
    assert!(report.foreign_instances.is_empty(), "{:?}", report.foreign_instances);
    let info = mgr.inspect(&sid, gen).unwrap();
    assert_eq!(
        info.state,
        SandboxState::Running,
        "the kernel still runs it — the record alone was never proof (notes: {:?})",
        report.notes
    );
    let out = mgr
        .execute(
            &sid,
            gen,
            &ExecRequest {
                argv: vec!["/bin/dash".into(), "-c".into(), "echo C04_ALIVE".into()],
                env: vec![],
                cwd: None,
                timeout: Some(Duration::from_secs(15)),
            },
        )
        .unwrap();
    assert_eq!(out.stdout.trim(), "C04_ALIVE");
    eprintln!("T08-C04 evidence: reopen->exec OK wall={}ms", out.wall_ms);
    let cleanup = mgr.delete(&sid, gen).unwrap();
    assert!(cleanup.all_ok(), "{:?}", cleanup.failed_steps());
}

// silence unused warnings for helpers used only under gates
#[allow(dead_code)]
fn _unused(_: &Rig, _: &ManagerConfig) {}
