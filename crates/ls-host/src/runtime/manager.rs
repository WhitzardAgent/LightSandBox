//! `SandboxManager`: the application service driving one backend through
//! the real lifecycle — create / inspect / execute / delete — over
//! T06 images, T07 rootfs leases and the [`SandboxBackend`] boundary.
//!
//! Hard rules honoured here:
//! * readiness and liveness claims come ONLY from backend-observed kernel
//!   state / guest receipts — never from a record's self-claim
//!   ("服务就绪以真实状态为准");
//! * delete acceptance and final reclaim are recorded separately:
//!   the returned value is a real [`CleanupInfo`] (DEL-1) whose every step
//!   executed in this process;
//! * generations fence: owner check first, then incarnation fence
//!   (STALE_GENERATION); a retired id is never reusable (access.rs);
//! * a manager crash mid-create/mid-delete leaves only resources its
//!   journal can roll back or its restart reconcile can re-observe;
//! * requirement checks run BEFORE any host effect: an unsatisfiable hard
//!   requirement rejects via the shared planner (HOST_CAPABILITY_UNAVAILABLE),
//!   never silently degrades (docs/v5/08_FAILURES.md).

use crate::images::error::ImageError;
use crate::images::layout::load_verified;
use crate::images::process_config::{convert, ImageProcessConfig, ProcessOverrides};
use crate::images::store::ImageStore;
use crate::images::Platform;
use crate::rootfs::lease::{mount_leaks_under, sweep, GcReport};
use crate::rootfs::prepare::{Driver, RootfsManager};
use crate::runtime::backend::{
    BackendError, BackendExecSpec, BackendInstance, BackendSandboxSpec, BackendState,
    ExecTermination, InitReceipt, SandboxBackend,
};
use crate::runtime::cleanup::{unix_ms_now, CleanupInfo, CleanupStep};
use crate::runtime::journal::{Journal, RolledBack, StepOutcome};
use crate::runtime::native::{instance_label, parse_receipt, runc};
use ls_core::domain::access::{ExecutionBinding, SandboxLifetime, SandboxRecord};
use ls_core::domain::backend::{plan, BackendPreference, CapabilityRequirements};
use ls_core::domain::error::{DomainError, ErrorCode};
use ls_core::domain::execution::{ExitRecord, TerminationReason};
use ls_core::domain::ids::{ExecutionId, Generation, GuestPath, OwnerId, Revision, SandboxId};
use ls_core::domain::lifecycle::{ExecutionState, SandboxState};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ config

pub struct ManagerConfig {
    /// Registry root (records live in `<state_root>/instances`).
    pub state_root: PathBuf,
    /// ImageStore root (blobs/refs/… — T06).
    pub images_root: PathBuf,
    /// RootfsManager data root (bases + per-sandbox leases — T07).
    pub rootfs_root: PathBuf,
    /// Pinned unpack binary for base materialization.
    pub umoci_binary: PathBuf,
    /// Image platform gate (hard fail before any exec — T06-C03).
    pub host: Platform,
    /// Trusted principal for record fencing (the API layer's authenticated
    /// identity; v1 workers run single-principal).
    pub owner: OwnerId,
    /// When set, prepared lease trees are chowned to this identity before
    /// backend hand-off (the PORTABLE_ROOTLESS runtime user). None = run
    /// everything as the current uid (production unprivileged worker).
    pub runtime_identity: Option<(u32, u32)>,
}

pub struct CreateSandboxInput {
    /// Runnable store name (manifest hex, from ImageStore::list_runnable).
    pub image: String,
    pub workdir: Option<String>,
    pub env: Vec<(String, String)>,
    /// Hard capability requirements — checked via the shared planner
    /// BEFORE any effect (08_FAILURES: requirement != observation).
    pub requirements: CapabilityRequirements,
    pub preference: BackendPreference,
}

/// One authoritative execution request (contract execution vocabulary).
/// argv is explicit and non-empty: v1 never guesses a shell. env is
/// `KEY=VALUE` exec-local extras applied over the container config env.
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub timeout: Option<Duration>,
}

// ------------------------------------------------------------------ errors

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerErrorCode {
    InvalidArgument,
    NotFound,
    Conflict,
    StaleGeneration,
    /// Hard requirement rejected by the planner (HOST_CAPABILITY_UNAVAILABLE class).
    CapabilityUnavailable,
    Unavailable,
    Internal,
}

#[derive(Debug)]
pub struct ManagerError {
    pub code: ManagerErrorCode,
    pub message: String,
    /// Cleanup/compensation errors — surfaced WITH the original error,
    /// never instead of it.
    pub cleanup_errors: Vec<String>,
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)?;
        if !self.cleanup_errors.is_empty() {
            write!(f, " (cleanup errors: {:?})", self.cleanup_errors)?;
        }
        Ok(())
    }
}

impl std::error::Error for ManagerError {}

impl ManagerError {
    fn new(code: ManagerErrorCode, message: impl Into<String>) -> Self {
        ManagerError {
            code,
            message: message.into(),
            cleanup_errors: Vec::new(),
        }
    }
    fn from_domain(e: DomainError) -> Self {
        let code = match e.code {
            ErrorCode::InvalidArgument => ManagerErrorCode::InvalidArgument,
            ErrorCode::NotFound => ManagerErrorCode::NotFound,
            ErrorCode::Conflict => ManagerErrorCode::Conflict,
            ErrorCode::StaleGeneration => ManagerErrorCode::StaleGeneration,
            ErrorCode::Forbidden => ManagerErrorCode::NotFound, // never leak cross-owner existence
            _ => ManagerErrorCode::Internal,
        };
        ManagerError::new(code, e.message)
    }
    fn from_image(e: ImageError) -> Self {
        ManagerError::new(ManagerErrorCode::Unavailable, e.to_string())
    }
    fn from_backend(code: ManagerErrorCode, e: BackendError) -> Self {
        ManagerError {
            code,
            message: e.to_string(),
            cleanup_errors: e.cleanup_errors.clone(),
        }
    }
}

// ------------------------------------------------------------------ outputs

#[derive(Debug)]
pub struct CreatedSandbox {
    pub sandbox_id: SandboxId,
    pub generation: Generation,
    /// REAL state observed from the backend right after start (never
    /// "declared" running): Running, Stopped (short-lived entrypoint), …
    pub observed: SandboxState,
    pub instance_label: String,
    /// Planner decision actually applied (requirement resolution is
    /// visible, never silent).
    pub plan_selected_kind: ls_core::domain::backend::BackendKind,
    pub plan_reason: String,
}

pub struct SandboxInfo {
    pub record: SandboxRecord,
    /// Domain state after reconciling with the live observation.
    pub state: SandboxState,
    /// Live kernel/backend observation underlying it.
    pub observed_backend: BackendState,
    pub image_digest: String,
    pub instance_label: String,
    /// The guest init receipt of the entrypoint when written (the ONLY
    /// exit-code truth for the sandbox PID-1 child).
    pub init_receipt: Option<InitReceipt>,
    pub created_unix_ms: u64,
    /// Diagnostics (implementation name) — never a selection result.
    pub implementation: Option<String>,
}

#[derive(Debug)]
pub struct ExecutionOutcomeView {
    pub execution_id: ExecutionId,
    pub state: ExecutionState,
    pub exit_record: Option<ExitRecord>,
    pub termination: TerminationReason,
    pub stdout: String,
    pub stderr: String,
    pub wall_ms: u64,
    pub truncated: bool,
    pub audits: Vec<String>,
}

/// What a reopen found — the restart-reconciliation evidence (T08-C04 new
/// process is identified via THIS report + fresh observations).
pub struct ManagerOpenReport {
    /// Real T07 GC pass over the rootfs data root (active ids = current
    /// registry), run before any record is trusted.
    pub lease_gc: GcReport,
    /// Registry records re-observed against live kernel state.
    pub instances_seen: usize,
    /// Live backend instances WITHOUT a registry record (foreign or
    /// leftover) — reported, never silently adopted.
    pub foreign_instances: Vec<String>,
    pub notes: Vec<String>,
}

// ------------------------------------------------------------------ records

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstanceRecordDoc {
    version: u32,
    sandbox_id: String,
    owner_id: String,
    generation: u64,
    revision: u64,
    lifetime: String, // "ACTIVE" | "RETIRED"
    state: String,    // SandboxState wire
    label: String,
    image_digest: String,
    image_config_digest: String,
    default_cwd: String,
    default_env: Vec<String>,
    default_user: (u32, u32),
    hostname: String,
    lease_id: String,
    rootfs_dir: String,
    /// The T07 lease sandbox dir (parent of the rootfs) — the delete path's
    /// teardown + mount-leak-audit anchor. Older records lack it: teardown
    /// then falls back to the rootfs parent.
    #[serde(default)]
    sandbox_dir: String,
    created_unix_ms: u64,
    exec_counter: u64,
    cleanup_ids: Vec<String>,
    audits: Vec<String>,
}

impl InstanceRecordDoc {
    fn to_record(&self) -> Result<SandboxRecord, ManagerError> {
        let lifetime = match self.lifetime.as_str() {
            "ACTIVE" => SandboxLifetime::Active,
            "RETIRED" => SandboxLifetime::Retired,
            other => {
                return Err(ManagerError::new(
                    ManagerErrorCode::Internal,
                    format!("record has unknown lifetime {other:?}"),
                ))
            }
        };
        let id = SandboxId::new(&self.sandbox_id).map_err(ManagerError::from_domain)?;
        let owner = OwnerId::new(&self.owner_id).map_err(ManagerError::from_domain)?;
        Ok(SandboxRecord {
            id,
            owner,
            generation: Generation(self.generation),
            revision: Revision(self.revision),
            lifetime,
        })
    }

    fn domain_state(&self) -> Result<SandboxState, ManagerError> {
        SandboxState::ALL
            .iter()
            .find(|s| s.wire() == self.state)
            .copied()
            .ok_or_else(|| {
                ManagerError::new(
                    ManagerErrorCode::Internal,
                    format!("record has unknown sandbox state {:?}", self.state),
                )
            })
    }
}

// ------------------------------------------------------------ ID minting

static MINT_N: AtomicU64 = AtomicU64::new(0);

fn mint_id(prefix: &str) -> String {
    let n = MINT_N.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{n}-{nanos:x}", std::process::id())
}

/// The manager. Generic over the backend: the future UML Kernel backend
/// slots in without touching ANY logic below (ADR 0002 §7).
pub struct SandboxManager<B: SandboxBackend> {
    images: ImageStore,
    rootfs: RootfsManager,
    backend: B,
    config: ManagerConfig,
}

impl<B: SandboxBackend + Clone + Send + 'static> SandboxManager<B> {
    /// Open the manager over an EXISTING state root and reconcile records
    /// against live kernel state (T08-C04). Records are never trusted
    /// alone; every live-state claim below comes from a fresh observation.
    pub fn open(config: ManagerConfig, backend: B) -> Result<(Self, ManagerOpenReport), ManagerError> {
        let images = ImageStore::open(&config.images_root).map_err(ManagerError::from_image)?;
        let rootfs = RootfsManager::new(
            &config.rootfs_root,
            crate::images::unpack::UmociUnpacker::new(&config.umoci_binary),
        );
        fs::create_dir_all(config.state_root.join("instances")).map_err(|e| {
            ManagerError::new(
                ManagerErrorCode::Internal,
                format!("registry dir: {e}"),
            )
        })?;
        let mgr = SandboxManager {
            images,
            rootfs,
            backend,
            config,
        };

        let mut notes = Vec::new();
        // 1. T07 GC: previous crashed prepares and orphaned (record-less)
        //    leases are removed; mount state is verified BEFORE any rm
        //    (mount-leak honesty — sweep refuses on live mounts).
        let active_ids: Vec<String> = mgr
            .list_records()?
            .iter()
            .filter(|d| d.lifetime == "ACTIVE")
            .map(|d| d.sandbox_id.clone())
            .collect();
        let active_refs: Vec<&str> = active_ids.iter().map(String::as_str).collect();
        let lease_gc = sweep(&mgr.config.rootfs_root, &active_refs, Duration::ZERO)
            .map_err(ManagerError::from_image)?;
        if !lease_gc.removed.is_empty() || !lease_gc.reclaimed_committed.is_empty() {
            notes.push(format!(
                "lease-gc: removed={} reclaimed={} refused_mount_leak={}",
                lease_gc.removed.len(),
                lease_gc.reclaimed_committed.len(),
                lease_gc.refused_mount_leak.len()
            ));
        }
        for (path, err) in &lease_gc.errors {
            notes.push(format!("lease-gc error at {}: {err}", path.display()));
        }

        // 2. Re-observe every registered instance against kernel truth.
        let mut instances_seen = 0usize;
        for doc in mgr.list_records()? {
            instances_seen += 1;
            let state = doc.domain_state()?;
            if matches!(
                state,
                SandboxState::Created | SandboxState::Starting | SandboxState::Running
            ) {
                let instance = mgr.backend.handle(&doc.label);
                let observed = mgr
                    .backend
                    .state(&instance)
                    .unwrap_or(BackendState::Unknown);
                match observed {
                    BackendState::Running | BackendState::Created => {
                        notes.push(format!(
                            "reconcile: {} live (kernel {:?})",
                            doc.sandbox_id, observed
                        ));
                    }
                    BackendState::Stopped => {
                        notes.push(format!(
                            "reconcile: {} record said {:?}, kernel says stopped; reconciling",
                            doc.sandbox_id,
                            state.wire()
                        ));
                        let mut d = doc.clone();
                        let cur = d.domain_state()?;
                        if let Ok(path) = advance_chain(cur, SandboxState::Stopped) {
                            for hop in path {
                                d.state = hop.wire().to_string();
                            }
                            d.audits.push(
                                "reconcile: kernel instance stopped; receipt read at next inspect"
                                    .to_string(),
                            );
                            let _ = mgr.write_record(&d);
                        }
                    }
                    BackendState::NotFound | BackendState::Unknown => {
                        // The kernel has no trace but the record claims
                        // life: never fake a receipt — mark audit and leave
                        // the state projection to say "stopped" honestly.
                        notes.push(format!(
                            "reconcile: {} record said {:?}, kernel reports {:?}; reconciling to stopped WITHOUT exit receipt (exit truth unknown)",
                            doc.sandbox_id,
                            state.wire(),
                            observed
                        ));
                        let mut d = doc.clone();
                        if let Ok(path) = advance_chain(
                            doc.domain_state()?,
                            SandboxState::Stopped,
                        ) {
                            for hop in path {
                                d.state = hop.wire().to_string();
                            }
                            d.audits.push(
                                "reconcile: backend instance vanished while record was live; no exit receipt exists — exit is unknown by design"
                                    .to_string(),
                            );
                            let _ = mgr.write_record(&d);
                        }
                    }
                }
            }
        }

        // 3. Foreign live instances: known to the backend but not to the
        //    registry (leftover from another manager / earlier crash
        //    before record write). Reported, never adopted.
        let mut foreign_instances = Vec::new();
        for id in mgr.backend_live_ids()? {
            let known = mgr
                .list_records()?
                .iter()
                .any(|d| d.label == id);
            if !known && id.starts_with("lsbx-") {
                foreign_instances.push(id);
            }
        }
        for f in &foreign_instances {
            notes.push(format!("reconcile: foreign live instance {f} (no registry record)"));
        }

        Ok((
            mgr,
            ManagerOpenReport {
                lease_gc,
                instances_seen,
                foreign_instances,
                notes,
            },
        ))
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn backend_live_ids(&self) -> Result<Vec<String>, ManagerError> {
        // Default: backends don't all support enumeration; the Native one
        // re-exposes `runc list` through this hook below.
        Ok(self.backend_live_ids_impl())
    }

    fn backend_live_ids_impl(&self) -> Vec<String> {
        self.backend.enumerate_live()
    }

    // ------------------------------------------------------------ registry

    fn instances_dir(&self) -> PathBuf {
        self.config.state_root.join("instances")
    }

    fn record_dir(&self, id: &str) -> PathBuf {
        self.instances_dir().join(id)
    }

    fn list_records(&self) -> Result<Vec<InstanceRecordDoc>, ManagerError> {
        let mut out = Vec::new();
        let dir = self.instances_dir();
        if !dir.is_dir() {
            return Ok(out);
        }
        for ent in fs::read_dir(&dir).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("registry read: {e}"))
        })? {
            let ent = ent.map_err(|e| {
                ManagerError::new(ManagerErrorCode::Internal, format!("registry read: {e}"))
            })?;
            let path = ent.path().join("record.json");
            if path.is_file() {
                let text = fs::read_to_string(&path).map_err(|e| {
                    ManagerError::new(ManagerErrorCode::Internal, format!("record read: {e}"))
                })?;
                let doc: InstanceRecordDoc = serde_json::from_str(&text).map_err(|e| {
                    ManagerError::new(
                        ManagerErrorCode::Internal,
                        format!("record {} invalid: {e}", path.display()),
                    )
                })?;
                out.push(doc);
            }
        }
        Ok(out)
    }

    fn read_record(&self, id: &SandboxId) -> Result<InstanceRecordDoc, ManagerError> {
        let path = self.record_dir(id.as_str()).join("record.json");
        if !path.is_file() {
            return Err(ManagerError::new(
                ManagerErrorCode::NotFound,
                format!("sandbox {} not found", id.as_str()),
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("record read: {e}"))
        })?;
        serde_json::from_str(&text).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("record invalid: {e}"))
        })
    }

    fn write_record(&self, doc: &InstanceRecordDoc) -> Result<(), ManagerError> {
        let dir = self.record_dir(&doc.sandbox_id);
        fs::create_dir_all(&dir).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("registry mkdir: {e}"))
        })?;
        let tmp = dir.join(".record.json.tmp");
        let body = serde_json::to_vec_pretty(doc).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("record encode: {e}"))
        })?;
        fs::write(&tmp, &body).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("record write: {e}"))
        })?;
        fs::rename(&tmp, dir.join("record.json")).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("record publish: {e}"))
        })?;
        Ok(())
    }

    // ------------------------------------------------------------ create

    pub fn create(
        &mut self,
        input: &CreateSandboxInput,
    ) -> Result<CreatedSandbox, ManagerError> {
        let creation_started = Instant::now();
        let mut j = Journal::new();

        // 0. hard requirement resolution BEFORE any effect.
        let offers = [self.backend.capabilities()];
        let decided = plan(&input.requirements, input.preference, &offers).map_err(|rej| {
            ManagerError::new(
                ManagerErrorCode::CapabilityUnavailable,
                format!("{rej}"),
            )
        })?;
        if decided.selected != self.backend.kind() {
            return Err(ManagerError::new(
                ManagerErrorCode::CapabilityUnavailable,
                format!(
                    "planner selected {:?} but this build ships only the {:?} backend",
                    decided.selected,
                    self.backend.kind()
                ),
            ));
        }

        // 1. mint identity; collide-check against registry.
        let sandbox_id = SandboxId::new(mint_id("sbx")).map_err(ManagerError::from_domain)?;
        if self.record_dir(sandbox_id.as_str()).exists() {
            return Err(ManagerError::new(
                ManagerErrorCode::Conflict,
                format!("sandbox id collision: {}", sandbox_id.as_str()),
            ));
        }

        // 2. step "image-load-verify": materialize a throwaway OCI-layout
        //    view of the store image, then run the FULL T06 verification
        //    (digests + platform gate) and process-config resolution on it.
        //    Compensation removes the view; every validation failure rolls
        //    back with the original error intact.
        let sid = sandbox_id.as_str().to_string();
        let view_id = format!("{sid}-layout");
        let (view_dir, image, process, user) = j
            .step_value("image-load-verify", || {
                let view = self.materialize_layout_view(&input.image, &view_id)?;
                let undo = view.clone();
                let verified = (|| -> Result<(_, _, (u32, u32)), ManagerError> {
                    let image = load_verified(&view, Some("runtime"), &self.config.host)
                        .map_err(ManagerError::from_image)?;
                    let img_cfg = ImageProcessConfig::from_config(&image.config)
                        .map_err(ManagerError::from_image)?;
                    let overrides = ProcessOverrides {
                        cwd: input.workdir.clone(),
                        env: input.env.clone(),
                        ..ProcessOverrides::none()
                    };
                    let process =
                        convert(&img_cfg, &overrides).map_err(ManagerError::from_image)?;
                    if process.argv.is_empty() {
                        return Err(ManagerError::new(
                            ManagerErrorCode::InvalidArgument,
                            "image has neither entrypoint nor cmd and no argv override was given",
                        ));
                    }
                    let user = parse_user(process.user.as_deref())
                        .map_err(|e| ManagerError::new(ManagerErrorCode::InvalidArgument, e))?;
                    if user != (0, 0) {
                        // Single-ID rootless mapping: only guest uid/gid 0 is
                        // mapped. Anything else would be an unmapped-uid exec —
                        // reject honestly, never fail deep inside the runtime.
                        return Err(ManagerError::new(
                            ManagerErrorCode::CapabilityUnavailable,
                            format!(
                                "image user {} requires multi-uid mapping; the measured Native backend offers single-id only",
                                process.user.as_deref().unwrap_or("")
                            ),
                        ));
                    }
                    Ok((image, process, user))
                })();
                let (image, process, user) = verified?;
                let note = format!("verified {}", image.manifest_digest);
                Ok((
                    (view, image, process, user),
                    StepOutcome::with_compensation(Some(note), move || {
                        fs::remove_dir_all(&undo)
                            .map_err(|e| format!("remove layout view {}: {e}", undo.display()))
                    }),
                ))
            })
            .map_err(Self::rolled_back)?;

        let generation = Generation::INITIAL;
        let hostname = legal_hostname(&instance_label(sandbox_id.as_str(), generation.get()));

        // 3. step "prepare-lease": T07 real prepare (verified layout ->
        //    per-sandbox writable rootfs) + the PORTABLE_ROOTLESS chown
        //    handover. The lease OBJECT lives inside the compensation
        //    closure: a rollback releases it (mount-leak honesty inside
        //    release); a commit drops the object and keeps the on-disk
        //    lease — RootfsLease::Drop is deliberately inert, and the
        //    delete path re-derives teardown facts from the record.
        let (lease_id, sandbox_dir, rootfs_path): (String, PathBuf, PathBuf) = j
            .step_value("prepare-lease", || {
                let lease = self
                    .rootfs
                    .prepare(&view_dir, "runtime", &image, &sid, Driver::FullCopy)
                    .map_err(|e| {
                        ManagerError::new(
                            ManagerErrorCode::Unavailable,
                            format!("rootfs prepare: {e}"),
                        )
                    })?;
                if let Some((uid, gid)) = self.config.runtime_identity {
                    runc::chown_tree(lease.sandbox_dir(), uid, gid, 0o755).map_err(|e| {
                        ManagerError::new(
                            ManagerErrorCode::Unavailable,
                            format!("lease chown: {e}"),
                        )
                    })?;
                }
                let facts = (
                    lease.record().lease_id.clone(),
                    lease.sandbox_dir().to_path_buf(),
                    lease.record().rootfs.clone(),
                );
                let note = format!("lease {}", facts.0);
                Ok((
                    facts,
                    StepOutcome::with_compensation(Some(note), move || {
                        lease.release().map_err(|e| format!("lease release: {e}"))
                    }),
                ))
            })
            .map_err(Self::rolled_back)?;

        // 4. step "backend-create": every effect (bundle, uid-mapping,
        //    container registration) lives behind the backend boundary and
        //    the rollback goes through the SAME boundary (backend delete),
        //    never raw path surgery by the manager.
        let instance: BackendInstance = j
            .step_value("backend-create", || {
                let spec = BackendSandboxSpec {
                    sandbox_id: sandbox_id.clone(),
                    generation: generation.get(),
                    rootfs: rootfs_path.clone(),
                    hostname: hostname.clone(),
                    argv: process.argv.clone(),
                    env: process.env.clone(),
                    cwd: GuestPath::new(process.cwd.clone()).map_err(ManagerError::from_domain)?,
                    user,
                };
                let inst = self
                    .backend
                    .create(&spec)
                    .map_err(|e| ManagerError::from_backend(ManagerErrorCode::Unavailable, e))?;
                let undo = inst.clone();
                let backend = self.backend.clone();
                let note = format!("instance {}", inst.label);
                Ok((
                    inst,
                    StepOutcome::with_compensation(Some(note), move || {
                        backend
                            .delete(&undo)
                            .map_err(|e| format!("rollback backend create of {}: {e}", undo.label))
                    }),
                ))
            })
            .map_err(Self::rolled_back)?;

        // 5. step "backend-start": start-ok is NOT a readiness claim — the
        //    observation below is the only truth. Rollback tears the whole
        //    backend side down (idempotent delete through the boundary).
        j.step("backend-start", || {
            self.backend
                .start(&instance)
                .map_err(|e| ManagerError::from_backend(ManagerErrorCode::Unavailable, e))?;
            let undo = instance.clone();
            let backend = self.backend.clone();
            Ok(StepOutcome::with_compensation(None, move || {
                backend
                    .delete(&undo)
                    .map_err(|e| format!("rollback backend start of {}: {e}", undo.label))
            }))
        })
        .map_err(Self::rolled_back)?;

        // 6. Post-start observation: the ONLY readiness truth is the
        //    backend-observed kernel state ("服务就绪以真实状态为准"). A
        //    short-lived entrypoint landing in Stopped is a VALID create
        //    (T08-C02); a vanished instance after start-ok is a HARD FAILURE
        //    that aborts the create with full rollback.
        let observed_backend = self.backend.state(&instance).unwrap_or(BackendState::Unknown);
        let receipt = self.read_init_receipt(&rootfs_path);
        let (target, mut audits) = match observed_backend {
            BackendState::Running => (
                SandboxState::Running,
                vec!["post-start kernel observation: running".to_string()],
            ),
            BackendState::Stopped => {
                let mut a = vec![
                    "post-start kernel observation: stopped (short-lived entrypoint)".to_string(),
                ];
                match &receipt {
                    Some((_, r)) => a.push(format!(
                        "init receipt observed at create: termination {:?}",
                        r.termination
                    )),
                    None => a.push(
                        "kernel reports stopped but no init receipt exists yet — exit truth resolves at first inspect"
                            .to_string(),
                    ),
                }
                (SandboxState::Stopped, a)
            }
            BackendState::Created => {
                // runc start returned ok while the kernel still lists the
                // container as created: legal on the record (no hops), but
                // NEVER upgraded to running by declaration.
                (
                    SandboxState::Created,
                    vec![
                        "post-start kernel observation: still created (start not yet effective — record stays honest)"
                            .to_string(),
                    ],
                )
            }
            BackendState::NotFound | BackendState::Unknown => {
                let err = ManagerError::new(
                    ManagerErrorCode::Unavailable,
                    format!(
                        "start returned ok but kernel observation is {observed_backend:?}; aborting create (no phantom registration)"
                    ),
                );
                return Err(Self::rolled_back(
                    j.fail_and_rollback("post-start-observe".to_string(), err),
                ));
            }
        };
        audits.push(format!(
            "plan: {} (create wall {} ms)",
            decided.reason,
            creation_started.elapsed().as_millis()
        ));

        // 7. step "register": the durable record is written hop-by-legal-hop
        //    (revision-bumped), so every persisted intermediate state is
        //    contract-legal. Compensation: none — a partially persisted
        //    register is still a legal EARLIER state; resource rollback
        //    above + reconcile-at-open converge to one truth.
        let owner_id = self.config.owner.as_str().to_string();
        let image_digest = image.manifest_digest.clone();
        let image_config_digest = image.config_digest.clone();
        let label = instance.label.clone();
        let default_cwd = process.cwd.clone();
        let default_env = process.env.clone();
        j.step("register", || {
            let mut doc = InstanceRecordDoc {
                version: 1,
                sandbox_id: sid.clone(),
                owner_id,
                generation: generation.get(),
                revision: Revision::INITIAL.get(),
                lifetime: "ACTIVE".to_string(),
                state: SandboxState::Created.wire().to_string(),
                label: label.clone(),
                image_digest: image_digest.clone(),
                image_config_digest: image_config_digest.clone(),
                default_cwd: default_cwd.clone(),
                default_env: default_env.clone(),
                default_user: user,
                hostname: hostname.clone(),
                lease_id: lease_id.clone(),
                rootfs_dir: rootfs_path.display().to_string(),
                sandbox_dir: sandbox_dir.display().to_string(),
                created_unix_ms: unix_ms_now(),
                exec_counter: 0,
                cleanup_ids: Vec::new(),
                audits: audits.clone(),
            };
            self.advance_record(&mut doc, target, "create registered".to_string())?;
            Ok(StepOutcome::irreversible(Some(format!(
                "record {} revision {}",
                doc.sandbox_id, doc.revision
            ))))
        })
        .map_err(Self::rolled_back)?;

        let _events = j.commit();
        // The FullCopy lease is now authoritative: the throwaway view can
        // go (best effort; the delete path sweeps any leftover anyway).
        if let Err(e) = fs::remove_dir_all(&view_dir) {
            eprintln!(
                "WARNING: layout view {} not removed after commit: {e}",
                view_dir.display()
            );
        }

        Ok(CreatedSandbox {
            sandbox_id,
            generation,
            observed: target,
            instance_label: instance.label.clone(),
            plan_selected_kind: decided.selected,
            plan_reason: decided.reason.clone(),
        })
    }

    /// Fold a journaled rollback into ONE error surface: the original
    /// failure plus every compensation error, never one without the other.
    fn rolled_back(rb: RolledBack<ManagerError>) -> ManagerError {
        let mut e = rb.original;
        e.cleanup_errors = rb
            .compensation_errors
            .iter()
            .map(|(s, err)| format!("{s}: {err}"))
            .collect();
        e
    }

    /// Build `<state_root>/views/<view_id>`: a throwaway OCI-image-layout
    /// VIEW of one runnable store ref (`refs/<manifest-hex>`). The store is
    /// the trust anchor: the manifest bytes must sha256 to the ref name;
    /// config/layer blobs are hard-linked (copied as fallback) from the
    /// content-addressed store; the T06 verified loader then re-verifies
    /// EVERYTHING on this view. No store content is ever modified.
    fn materialize_layout_view(&self, image: &str, view_id: &str) -> Result<PathBuf, ManagerError> {
        let manifest_path = self
            .images
            .root()
            .join("refs")
            .join(image)
            .join("manifest.json");
        if !manifest_path.is_file() {
            return Err(ManagerError::new(
                ManagerErrorCode::NotFound,
                format!(
                    "image {image:?} is not a runnable store ref (missing {})",
                    manifest_path.display()
                ),
            ));
        }
        let manifest = fs::read(&manifest_path).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Unavailable, format!("manifest read: {e}"))
        })?;
        let hex = ls_core::domain::idempotency::sha256_hex(&manifest);
        if hex != image {
            return Err(ManagerError::new(
                ManagerErrorCode::Unavailable,
                format!("store corruption: refs/{image}/manifest.json hashes to {hex}"),
            ));
        }
        let view = self.config.state_root.join("views").join(view_id);
        if view.exists() {
            // Clobber law applies to the manager too: a leftover view dir
            // is never silently erased — the delete path owns removal, and
            // a collision with a live sibling must fail loudly.
            return Err(ManagerError::new(
                ManagerErrorCode::Conflict,
                format!("layout view already exists: {} (leftover?)", view.display()),
            ));
        }
        let blobs = view.join("blobs").join("sha256");
        fs::create_dir_all(&blobs).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("view mkdir: {e}"))
        })?;
        let build = (|| -> Result<(), ManagerError> {
            fs::write(view.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)
                .map_err(|e| {
                    ManagerError::new(ManagerErrorCode::Internal, format!("oci-layout write: {e}"))
                })?;
            let parsed: serde_json::Value = serde_json::from_slice(&manifest).map_err(|e| {
                ManagerError::new(ManagerErrorCode::Unavailable, format!("manifest json: {e}"))
            })?;
            let media_type = parsed
                .get("mediaType")
                .and_then(|m| m.as_str())
                .unwrap_or("application/vnd.oci.image.manifest.v1+json")
                .to_string();
            let mut wanted: Vec<String> = Vec::new();
            if let Some(c) = parsed
                .get("config")
                .and_then(|c| c.get("digest"))
                .and_then(|d| d.as_str())
            {
                wanted.push(c.to_string());
            }
            if let Some(layers) = parsed.get("layers").and_then(|l| l.as_array()) {
                for l in layers {
                    if let Some(d) = l.get("digest").and_then(|d| d.as_str()) {
                        wanted.push(d.to_string());
                    }
                }
            }
            // manifest blob itself (small: a copy keeps the view decoupled
            // from store churn)
            fs::write(blobs.join(&hex), &manifest).map_err(|e| {
                ManagerError::new(ManagerErrorCode::Internal, format!("manifest blob write: {e}"))
            })?;
            for d in &wanted {
                let bh = d.strip_prefix("sha256:").ok_or_else(|| {
                    ManagerError::new(
                        ManagerErrorCode::Unavailable,
                        format!("non-sha256 digest {d:?} in manifest"),
                    )
                })?;
                let src = self.images.root().join("blobs").join("sha256").join(bh);
                let dst = blobs.join(bh);
                if !src.is_file() {
                    return Err(ManagerError::new(
                        ManagerErrorCode::Unavailable,
                        format!("store corruption: blob {} missing for refs/{image}", src.display()),
                    ));
                }
                if fs::hard_link(&src, &dst).is_err() {
                    fs::copy(&src, &dst).map_err(|e| {
                        ManagerError::new(ManagerErrorCode::Internal, format!("blob copy {bh}: {e}"))
                    })?;
                }
            }
            let index = serde_json::json!({
                "schemaVersion": 2,
                "manifests": [{
                    "mediaType": media_type,
                    "digest": format!("sha256:{hex}"),
                    "size": manifest.len() as u64,
                    "annotations": {"org.opencontainers.image.ref.name": "runtime"}
                }]
            });
            fs::write(view.join("index.json"), serde_json::to_vec_pretty(&index).unwrap())
                .map_err(|e| {
                    ManagerError::new(ManagerErrorCode::Internal, format!("index write: {e}"))
                })?;
            Ok(())
        })();
        if let Err(e) = build {
            // OUR build failed on OUR fresh dir: removing it is not
            // clobbering — nothing else can own a dir we just created.
            let _ = fs::remove_dir_all(&view);
            return Err(e);
        }
        Ok(view)
    }

    /// Raw bytes + strict parse of the guest init receipt inside OUR lease
    /// rootfs (`/.lsbx/receipt.json` — the manager-level host/guest
    /// protocol the native backend injected init honours). None = not
    /// written yet or malformed — never fabricated; the kernel state
    /// distinguishes "init done" from "receipt missing".
    fn read_init_receipt(&self, rootfs: &Path) -> Option<(String, InitReceipt)> {
        let text = fs::read_to_string(rootfs.join(".lsbx").join("receipt.json")).ok()?;
        let parsed = parse_receipt(&text)?;
        Some((text, parsed))
    }

    /// Persist contract-legal state hops for an existing record, bumping
    /// the revision at EVERY hop; the audit line lands with the first
    /// persisted write.
    fn advance_record(
        &self,
        doc: &mut InstanceRecordDoc,
        target: SandboxState,
        audit: String,
    ) -> Result<(), ManagerError> {
        let from = doc.domain_state()?;
        doc.audits.push(audit);
        if from == target {
            doc.revision = Revision(doc.revision)
                .checked_next()
                .map_err(ManagerError::from_domain)?
                .get();
            self.write_record(doc)?;
            return Ok(());
        }
        let hops = advance_chain(from, target).map_err(ManagerError::from_domain)?;
        for hop in hops {
            doc.state = hop.wire().to_string();
            doc.revision = Revision(doc.revision)
                .checked_next()
                .map_err(ManagerError::from_domain)?
                .get();
            self.write_record(doc)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ inspect

    /// Read-side reconciliation: the answer confronts the record with one
    /// FRESH backend observation before speaking. A live record whose
    /// kernel instance is stopped/vanished reconciles to stopped — with
    /// the exit receipt when it exists, with an explicit "exit unknown"
    /// audit when it does not (never a fabricated exit).
    pub fn inspect(&self, id: &SandboxId, generation: Generation) -> Result<SandboxInfo, ManagerError> {
        let mut doc = self.read_record(id)?;
        doc.to_record()?
            .authorize(&self.config.owner, Some(generation))
            .map_err(ManagerError::from_domain)?;
        let instance = self.backend.handle(&doc.label);
        let observed = self.backend.state(&instance).unwrap_or(BackendState::Unknown);
        let state = doc.domain_state()?;
        if matches!(
            state,
            SandboxState::Created | SandboxState::Starting | SandboxState::Running | SandboxState::Stopping
        ) {
            match observed {
                BackendState::Running => {
                    if state != SandboxState::Running {
                        self.advance_record(
                            &mut doc,
                            SandboxState::Running,
                            "inspect: kernel instance live; record reconciled to running"
                                .to_string(),
                        )?;
                    }
                }
                BackendState::Stopped => {
                    self.advance_record(
                        &mut doc,
                        SandboxState::Stopped,
                        "inspect: kernel instance stopped; the init receipt is the exit truth"
                            .to_string(),
                    )?;
                }
                BackendState::NotFound | BackendState::Unknown => {
                    self.advance_record(
                        &mut doc,
                        SandboxState::Stopped,
                        "inspect: backend instance vanished while the record was live; NO exit receipt is guaranteed — exit truth unknown by design"
                            .to_string(),
                    )?;
                }
                BackendState::Created => {}
            }
        }
        let receipt = self
            .read_init_receipt(&PathBuf::from(&doc.rootfs_dir))
            .map(|(_, parsed)| parsed);
        Ok(SandboxInfo {
            state: doc.domain_state()?,
            observed_backend: observed,
            image_digest: doc.image_digest.clone(),
            instance_label: doc.label.clone(),
            init_receipt: receipt,
            created_unix_ms: doc.created_unix_ms,
            record: doc.to_record()?,
            implementation: self.backend.implementation(),
        })
    }

    // ------------------------------------------------------------ execute

    /// One synchronous authoritative execution. The execution is bound to
    /// (sandbox, generation) in the record BEFORE the backend runs it
    /// (revision-bumped registration); the observed termination maps to the
    /// domain's mutually-exclusive exit record — a timeout yields
    /// ExitRecord::Unknown, never a fabricated exit code.
    pub fn execute(
        &self,
        id: &SandboxId,
        generation: Generation,
        req: &ExecRequest,
    ) -> Result<ExecutionOutcomeView, ManagerError> {
        let mut doc = self.read_record(id)?;
        let record = doc.to_record()?;
        record
            .authorize(&self.config.owner, Some(generation))
            .map_err(ManagerError::from_domain)?;
        if req.argv.is_empty() {
            return Err(ManagerError::new(
                ManagerErrorCode::InvalidArgument,
                "execution argv must not be empty (v1 never guesses a shell)",
            ));
        }
        let state = doc.domain_state()?;
        let instance = self.backend.handle(&doc.label);
        let observed = self.backend.state(&instance).unwrap_or(BackendState::Unknown);
        if state != SandboxState::Running || observed != BackendState::Running {
            return Err(ManagerError::new(
                ManagerErrorCode::Conflict,
                format!(
                    "exec requires a RUNNING sandbox; record={}, kernel={observed:?}",
                    state.wire()
                ),
            ));
        }
        let execution_id = ExecutionId::new(mint_id("exec")).map_err(ManagerError::from_domain)?;
        let binding = ExecutionBinding::new(execution_id.clone(), &record);
        let mut audits = vec![format!(
            "execution {} bound to sandbox {} generation {}",
            binding.execution_id.as_str(),
            binding.sandbox_id.as_str(),
            binding.sandbox_generation.get()
        )];
        doc.exec_counter += 1;
        doc.audits.push(format!("execution {} started", execution_id.as_str()));
        doc.revision = Revision(doc.revision)
            .checked_next()
            .map_err(ManagerError::from_domain)?
            .get();
        self.write_record(&doc)?;

        let spec = BackendExecSpec {
            argv: req.argv.clone(),
            env: req.env.clone(),
            cwd: match &req.cwd {
                Some(c) => Some(GuestPath::new(c).map_err(ManagerError::from_domain)?),
                None => None,
            },
            timeout: req.timeout,
        };
        let outcome = self
            .backend
            .exec(&instance, &spec)
            .map_err(|e| ManagerError::from_backend(ManagerErrorCode::Unavailable, e))?;
        if outcome.truncated {
            audits.push(
                "output exceeded the capture cap; the retained prefix is flagged truncated"
                    .to_string(),
            );
        }
        let (exec_state, exit_record, termination) = match outcome.termination {
            ExecTermination::Exited(code) => (
                ExecutionState::Exited,
                Some(ExitRecord::Exited { exit_code: code }),
                TerminationReason::None,
            ),
            ExecTermination::Timeout => (
                // The local hold was killed at the budget; the guest-side
                // process fate is NOT observed on this path — Unknown by
                // design, priced loudly in termination.
                ExecutionState::Unknown,
                Some(ExitRecord::Unknown),
                TerminationReason::Timeout,
            ),
        };
        Ok(ExecutionOutcomeView {
            execution_id,
            state: exec_state,
            exit_record,
            termination,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            wall_ms: outcome.wall_ms,
            truncated: outcome.truncated,
            audits,
        })
    }

    // ------------------------------------------------------------ delete

    /// DEL-1: acceptance and final reclaim are recorded SEPARATELY. Every
    /// reclaim step really executes here (wall-timed CleanupSteps): exit
    /// truth archived first, teardown through the backend boundary, lease
    /// release with the mount-leak audit, residue + view sweep. A failed
    /// step lands the sandbox in CleanupRequired — never a falsely clean
    /// Deleted — with the failed steps named in the audit; the returned
    /// CleanupInfo carries the full ledger either way.
    pub fn delete(&self, id: &SandboxId, generation: Generation) -> Result<CleanupInfo, ManagerError> {
        let mut doc = self.read_record(id)?;
        doc.to_record()?
            .authorize(&self.config.owner, Some(generation))
            .map_err(ManagerError::from_domain)?;
        if doc.domain_state()? == SandboxState::Deleted {
            return Err(ManagerError::new(
                ManagerErrorCode::Conflict,
                "sandbox is already deleted (its cleanup record is retained)",
            ));
        }
        let started_ms = unix_ms_now();
        let mut steps: Vec<CleanupStep> = Vec::new();
        let instance = self.backend.handle(&doc.label);
        let rootfs = PathBuf::from(&doc.rootfs_dir);
        let sandbox_dir = if doc.sandbox_dir.is_empty() {
            rootfs.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| rootfs.clone())
        } else {
            PathBuf::from(&doc.sandbox_dir)
        };

        // acceptance marker: revision-bumped, no state hop (reclaim below
        // writes the real hops AFTER the kernel-side effects happened).
        let accepted_at = doc.domain_state()?;
        self.advance_record(
            &mut doc,
            accepted_at,
            "delete accepted; reclaim begins (in-flight ops are null in v1 — no separate exec store, never claimed zero)"
                .to_string(),
        )?;

        // s1: archive the exit truth BEFORE any teardown.
        let t = Instant::now();
        let mut archived = None;
        match self.read_init_receipt(&rootfs) {
            Some((raw, parsed)) => {
                steps.push(CleanupStep {
                    name: "archive-init-receipt".to_string(),
                    ok: true,
                    note: Some(format!("termination {:?}", parsed.termination)),
                    wall_ms: t.elapsed().as_millis() as u64,
                });
                archived = Some(raw);
            }
            None => steps.push(CleanupStep {
                name: "archive-init-receipt".to_string(),
                ok: true,
                note: Some("no init receipt existed (never fabricated)".to_string()),
                wall_ms: t.elapsed().as_millis() as u64,
            }),
        }

        // s2: backend teardown (kill every task, delete the registration
        //     and the bundle) — all through the boundary.
        let t = Instant::now();
        match self.backend.delete(&instance) {
            Ok(()) => steps.push(CleanupStep {
                name: "backend-delete".to_string(),
                ok: true,
                note: None,
                wall_ms: t.elapsed().as_millis() as u64,
            }),
            Err(e) => steps.push(CleanupStep {
                name: "backend-delete".to_string(),
                ok: false,
                note: Some(format!("{e}")),
                wall_ms: t.elapsed().as_millis() as u64,
            }),
        }

        // s3: backend residue audit + sweep (the backend is authoritative
        //     for its own layout: it names the paths, we remove and report).
        let t = Instant::now();
        let mut residue_fail: Vec<String> = Vec::new();
        for p in self.backend.residue_paths(&instance) {
            if p.symlink_metadata().is_ok() {
                let r = if p.is_dir() {
                    fs::remove_dir_all(&p)
                } else {
                    fs::remove_file(&p)
                };
                if let Err(e) = r {
                    residue_fail.push(format!("{}: {e}", p.display()));
                }
            }
        }
        steps.push(CleanupStep {
            name: "backend-residue-sweep".to_string(),
            ok: residue_fail.is_empty(),
            note: if residue_fail.is_empty() {
                None
            } else {
                Some(format!("residue left: {}", residue_fail.join("; ")))
            },
            wall_ms: t.elapsed().as_millis() as u64,
        });

        // s4: lease teardown with mount-leak honesty (this mirrors
        //     RootfsLease::release; the lease object died with the create
        //     journal by design — the on-disk lease + this audit are the
        //     truth now). A live mount REFUSES removal. Idempotent: a lease
        //     dir already reclaimed by an EARLIER degraded delete is done,
        //     not an error (cleanup retries must converge).
        let t = Instant::now();
        let (lease_ok, lease_note) = if sandbox_dir.symlink_metadata().is_err() {
            (true, Some("lease already gone (earlier reclaim)".to_string()))
        } else {
            match mount_leaks_under(&sandbox_dir) {
            Ok(leaks) if !leaks.is_empty() => (
                false,
                Some(format!(
                    "REFUSED: live mount leaks under {}: {:?}",
                    sandbox_dir.display(),
                    leaks
                )),
            ),
            Ok(_) => match fs::remove_dir_all(&sandbox_dir) {
                Ok(()) => (true, None),
                Err(e) => (false, Some(format!("remove {}: {e}", sandbox_dir.display()))),
            },
                Err(e) => (false, Some(format!("mount audit failed: {e}"))),
            }
        };
        steps.push(CleanupStep {
            name: "lease-teardown".to_string(),
            ok: lease_ok,
            note: lease_note,
            wall_ms: t.elapsed().as_millis() as u64,
        });

        // s5: the throwaway layout view.
        let t = Instant::now();
        let view = self
            .config
            .state_root
            .join("views")
            .join(format!("{}-layout", doc.sandbox_id));
        let (view_ok, view_note) = if view.symlink_metadata().is_ok() {
            match fs::remove_dir_all(&view) {
                Ok(()) => (true, None),
                Err(e) => (false, Some(format!("remove {}: {e}", view.display()))),
            }
        } else {
            (true, Some("view already gone".to_string()))
        };
        steps.push(CleanupStep {
            name: "layout-view-remove".to_string(),
            ok: view_ok,
            note: view_note,
            wall_ms: t.elapsed().as_millis() as u64,
        });

        let all_ok = steps.iter().all(|s| s.ok);
        let cleanup_id = mint_id("cln");
        let failed_names: Vec<String> = steps
            .iter()
            .filter(|s| !s.ok)
            .map(|s| s.name.clone())
            .collect();
        let info = CleanupInfo {
            cleanup_id: cleanup_id.clone(),
            target_sandbox_id: id.as_str().to_string(),
            target_generation: generation.get(),
            scope: "sandbox".to_string(),
            ops_terminated: None,
            archived_init_receipt: archived,
            compaction_result: None,
            reversible: false,
            steps,
            started_unix_ms: started_ms,
            finished_unix_ms: unix_ms_now(),
        };
        // Persist the cleanup ledger BEFORE the final state hops: a crash
        // here leaves the ledger + a mid-hop record, both reconcilable.
        let cleanup_path = self
            .record_dir(id.as_str())
            .join(format!("cleanup-{cleanup_id}.json"));
        let body = serde_json::to_vec_pretty(&info).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("cleanup encode: {e}"))
        })?;
        let tmp = cleanup_path.with_extension("json.tmp");
        fs::write(&tmp, &body).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("cleanup write: {e}"))
        })?;
        fs::rename(&tmp, &cleanup_path).map_err(|e| {
            ManagerError::new(ManagerErrorCode::Internal, format!("cleanup publish: {e}"))
        })?;
        doc.cleanup_ids.push(cleanup_id);
        let final_state = if all_ok {
            doc.lifetime = "RETIRED".to_string();
            SandboxState::Deleted
        } else {
            SandboxState::CleanupRequired
        };
        let audit = if all_ok {
            "reclaim complete; all cleanup steps ok".to_string()
        } else {
            format!("reclaim DEGRADED — failed steps: {}", failed_names.join(", "))
        };
        // The lifetime flip is part of the doc and lands with the hops.
        self.advance_record(&mut doc, final_state, audit)?;
        Ok(info)
    }
}

/// Sandbox-id-safe hostname (RFC1123-ish); deterministic per instance.
fn legal_hostname(label: &str) -> String {
    let mut h: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    h.truncate(63);
    h
}

/// Numeric "uid[:gid]" parse; names are honestly rejected in v1 (single-id
/// rootless mapping has no /etc/passwd lookup semantics worth faking).
fn parse_user(user: Option<&str>) -> Result<(u32, u32), String> {
    match user {
        None => Ok((0, 0)),
        Some(u) => {
            let (uid_s, gid_s) = match u.split_once(':') {
                Some((a, b)) => (a, b),
                None => (u, u),
            };
            let uid: u32 = uid_s
                .parse()
                .map_err(|_| format!("image user {u:?} is not numeric (uid/gid names unsupported in v1)"))?;
            let gid: u32 = gid_s
                .parse()
                .map_err(|_| format!("image user {u:?} is not numeric (uid/gid names unsupported in v1)"))?;
            Ok((uid, gid))
        }
    }
}

/// Compute the legal hop chain from `from` to `to` (single steps only, so
/// every persisted intermediate state is contract-legal).
pub fn advance_chain(from: SandboxState, to: SandboxState) -> Result<Vec<SandboxState>, DomainError> {
    if from == to {
        return Ok(vec![]);
    }
    // BFS over the legal single-step table; the graph is tiny (linear prev
    // list — SandboxState carries no Ord bound).
    let mut prev: Vec<(SandboxState, Option<SandboxState>)> = vec![(from, None)];
    let mut frontier = vec![from];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for cur in frontier.drain(..) {
            for cand in SandboxState::ALL {
                if cur.can_transition(*cand) && !prev.iter().any(|(s, _)| s == cand) {
                    prev.push((*cand, Some(cur)));
                    if *cand == to {
                        let mut hops = vec![to];
                        let mut back = Some(cur);
                        while let Some(p) = back {
                            if p == from {
                                break;
                            }
                            hops.push(p);
                            back = prev
                                .iter()
                                .find(|(s, _)| *s == p)
                                .and_then(|(_, b)| *b);
                        }
                        hops.reverse();
                        return Ok(hops);
                    }
                    next.push(*cand);
                }
            }
        }
        frontier = next;
    }
    Err(DomainError::new(
        ErrorCode::Conflict,
        format!("no legal lifecycle path {} -> {}", from.wire(), to.wire()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_parse_numeric_only() {
        assert_eq!(parse_user(None), Ok((0, 0)));
        assert_eq!(parse_user(Some("0:0")), Ok((0, 0)));
        assert_eq!(parse_user(Some("1000:1000")), Ok((1000, 1000)));
        assert_eq!(parse_user(Some("1000")), Ok((1000, 1000)));
        assert!(parse_user(Some("root")).is_err());
        assert!(parse_user(Some("1000:staff")).is_err());
    }

    #[test]
    fn hostname_is_legal_and_deterministic() {
        assert_eq!(legal_hostname("lsbx-sbx_01-g1"), "lsbx-sbx-01-g1");
        assert!(legal_hostname("a.b").len() <= 63);
        assert!(legal_hostname("x").chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn lifecycle_chains_single_steps() {
        assert_eq!(
            advance_chain(SandboxState::Created, SandboxState::Running).unwrap(),
            vec![SandboxState::Starting, SandboxState::Running]
        );
        assert_eq!(
            advance_chain(SandboxState::Running, SandboxState::Stopped).unwrap(),
            vec![SandboxState::Stopped]
        );
        // Running→Stopped is a legal DIRECT edge in the frozen table
        // (short-lived entrypoint / crash): the shortest legal chain is
        // therefore stopped → deleting → deleted.
        assert_eq!(
            advance_chain(SandboxState::Running, SandboxState::Deleted).unwrap(),
            vec![
                SandboxState::Stopped,
                SandboxState::Deleting,
                SandboxState::Deleted
            ]
        );
        assert!(advance_chain(SandboxState::Deleted, SandboxState::Running).is_err());
    }
}
