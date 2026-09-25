//! The `SandboxBackend` boundary (ADR 0002 §7):
//!
//! ```text
//! SandboxManager (runtime/manager.rs)
//!   -> SandboxBackend  (this trait; backend-neutral types only)
//!     -> NativeRuncBackend (runtime/native/)
//!       -> RuncRuntime     (runtime/native/runc.rs)
//! ```
//!
//! A future Kernel/Machine backend implements THIS trait; the manager and
//! all domain logic stay untouched. Everything crossing the boundary is
//! typed against ls-core identities — implementation binary names appear
//! only inside backend-private code and the one `implementation`
//! diagnostics string (ADR 0002: diagnostics are never selection keys).

use ls_core::domain::backend::{BackendCapabilities, BackendKind};
use ls_core::domain::ids::{GuestPath, SandboxId};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// What a backend needs to bring one sandbox incarnation up. Built by the
/// manager from the verified image process config + caller overrides; the
/// image process argv is already resolved (entrypoint ++ cmd), env is the
/// exact authoritative list, guest state starts empty (docs/v5/03_DOMAIN.md
/// §进程模型 — argv is primary, no daemon config seeding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSandboxSpec {
    pub sandbox_id: SandboxId,
    pub generation: u64,
    /// Host path of the per-sandbox writable rootfs (T07 lease root).
    pub rootfs: PathBuf,
    /// UTS hostname inside the sandbox.
    pub hostname: String,
    /// Fully-resolved image process argv.
    pub argv: Vec<String>,
    /// Exact env (authoritative list, image order + overrides).
    pub env: Vec<String>,
    /// Guest working directory (default "/" if unset by image+caller).
    pub cwd: GuestPath,
    /// Image user, parsed "uid:gid" (both numeric in v1).
    pub user: (u32, u32),
}

/// Opaque handle for one backend-side instance. `label` is durable across
/// manager restarts: a reopened manager rebuilds the handle from its
/// registry and re-asks the backend for REAL kernel state — the record
/// alone is never proof of life (T08-C04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInstance {
    pub label: String,
}

/// Kernel-observed state of one instance — observed NOW by the backend,
/// never a cached claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendState {
    /// Created but not started.
    Created,
    /// Init process exists and runs.
    Running,
    /// Init process has exited (receipt carries the truth, when written).
    Stopped,
    /// No trace of the instance in kernel state.
    NotFound,
    /// State inspection failed ambiguously — caller must not guess.
    Unknown,
}

impl BackendState {
    pub fn is_live(self) -> bool {
        matches!(self, BackendState::Created | BackendState::Running)
    }
}

/// Termination of one exec, as observed on the direct hold path.
/// Maps to the domain's mutually-exclusive ExitRecord: an `Exited` code
/// comes from the real wait status; `Timeout` yields ExitRecord::Unknown
/// at the domain layer — never a fabricated code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecTermination {
    /// The exec'd process reaped normally with this code.
    Exited(i32),
    /// The wait budget expired; the local hold was killed. The remote
    /// process fate is delegated to the enclosing sandbox's teardown.
    Timeout,
}

/// One authoritative argv execution (runc-exec-class hold: stdout/stderr
/// captured from real pipes, exit observed on the waiting path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendExecSpec {
    pub argv: Vec<String>,
    /// Extra env applied to this exec only (image env is inherited by the
    /// container config; `KEY=VALUE` form).
    pub env: Vec<String>,
    /// Working dir override; None keeps the sandbox default.
    pub cwd: Option<GuestPath>,
    /// Wait budget. None = no deadline (caller chose; the API layer still
    /// bounds per-sandbox concurrency, not duration — contract 06).
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendExecOutcome {
    pub termination: ExecTermination,
    pub stdout: String,
    pub stderr: String,
    pub wall_ms: u64,
    /// true when captured output hit the capture cap and was truncated —
    /// never silently.
    pub truncated: bool,
}

/// The backend-facing per-sandbox init receipt: the exit of the sandbox's
/// PID-1 child (image entrypoint), observed by the guest init's own
/// waitpid and atomically persisted inside the rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReceipt {
    pub sandbox_id: String,
    pub child_pid: Option<i64>,
    pub termination: InitReceiptTermination,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitReceiptTermination {
    Exited { exit_code: i64 },
    Signaled { signal: i64 },
    /// exec of the entrypoint failed inside init (never ran).
    SpawnFailed { detail: String },
    /// The init's wait invariant broke (ECHILD before child seen).
    WaitInvariantBroken { detail: String },
}

/// Failure of a backend operation. `stage` names one stable step;
/// `detail` + `exit_code` + stderr tail are implementation diagnostics
/// (this is the one place binary names may appear).
#[derive(Debug, Clone)]
pub struct BackendError {
    pub stage: &'static str,
    pub detail: String,
    /// Errors already raised while compensating an earlier failure —
    /// surfaced WITH the original, never instead of it.
    pub cleanup_errors: Vec<String>,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.stage, self.detail)?;
        if !self.cleanup_errors.is_empty() {
            write!(f, " (+ {} cleanup errors)", self.cleanup_errors.len())?;
        }
        Ok(())
    }
}

impl std::error::Error for BackendError {}

impl BackendError {
    pub fn new(stage: &'static str, detail: impl Into<String>) -> Self {
        BackendError {
            stage,
            detail: detail.into(),
            cleanup_errors: Vec::new(),
        }
    }
    pub fn with_cleanup(mut self, cleanup_errors: Vec<String>) -> Self {
        self.cleanup_errors = cleanup_errors;
        self
    }
}

/// The boundary. Implementations: `native::NativeRuncBackend` (now);
/// a UML `Kernel` backend (future) — same trait, untouched manager.
pub trait SandboxBackend {
    fn kind(&self) -> BackendKind;

    /// Diagnostics-only implementation string ("runc 1.5.0-rc.1+dev…").
    /// Never a selection key (ADR 0002 §7).
    fn implementation(&self) -> Option<String>;

    /// Measured per-dimension capability offer of this backend on this
    /// host (spike-derived facts in v1; live probes in later tasks). Every
    /// declared dimension is present; unsupported stays visible.
    fn capabilities(&self) -> BackendCapabilities;

    /// Durable handle reconstruction (manager restart path — no kernel I/O).
    fn handle(&self, label: &str) -> BackendInstance;

    /// Create the instance from a fully-resolved spec. On success the
    /// instance exists and is startable; on failure the implementation has
    /// already rolled back everything IT created (bundle, container
    /// registrations) and reports original+cleanup errors together.
    fn create(&self, spec: &BackendSandboxSpec) -> Result<BackendInstance, BackendError>;

    /// Start the instance's init. Beware "start OK then instantly stopped"
    /// is a VALID outcome (short-lived entrypoint) — readiness claims are
    /// always made from `state`, never from start's exit alone.
    fn start(&self, instance: &BackendInstance) -> Result<(), BackendError>;

    /// Real kernel state, now.
    fn state(&self, instance: &BackendInstance) -> Result<BackendState, BackendError>;

    /// Run one argv execution against a live instance; authoritative
    /// stdout/stderr and termination. This call blocks for the execution.
    fn exec(
        &self,
        instance: &BackendInstance,
        exec: &BackendExecSpec,
    ) -> Result<BackendExecOutcome, BackendError>;

    /// The guest init's persisted receipt for the entrypoint, if written.
    /// None = no receipt (yet, or init never wrote one — callers use
    /// `state` to distinguish).
    fn init_receipt(&self, instance: &BackendInstance) -> Result<Option<InitReceipt>, BackendError>;

    /// Kill every task of the instance and delete all backend-side
    /// artifacts (container registration, bundle dir). Idempotent:
    /// deleting something already gone is Ok.
    fn delete(&self, instance: &BackendInstance) -> Result<(), BackendError>;

    /// Everything behind the boundary must expose its residue view: what
    /// backend-owned paths exist for this instance label (bundle dir etc).
    /// Used by the residue audit; the backend is authoritative for its own
    /// layout.
    fn residue_paths(&self, instance: &BackendInstance) -> Vec<PathBuf>;

    /// All LIVE instance labels this backend currently knows about (real
    /// kernel/registration state — used by restart reconciliation to spot
    /// instances that have no manager record). Default: the backend does
    /// not support enumeration (reconciliation then skips this check).
    fn enumerate_live(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `BTreeMap` merge helper kept here because both Native and (future) UML
/// exec paths apply per-exec env over a base list with identical key-replace
/// semantics (`KEY=VALUE` form, image order preserved).
pub fn overlay_env(base: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = base.to_vec();
    for kv in extra {
        let Some(eq) = kv.find('=') else { continue };
        let key = &kv[..eq];
        match out.iter().position(|e| e[..e.find('=').unwrap_or(e.len())] == *key) {
            Some(i) => out[i] = kv.clone(),
            None => out.push(kv.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_env_replaces_in_place_appends_new() {
        let base = vec!["PATH=/usr/bin".to_string(), "A=1".to_string()];
        let extra = vec!["A=2".to_string(), "B=".to_string()];
        let merged = overlay_env(&base, &extra);
        assert_eq!(
            merged,
            vec!["PATH=/usr/bin".to_string(), "A=2".to_string(), "B=".to_string()]
        );
    }

    #[test]
    fn backend_error_display_reports_cleanup_errors() {
        let e = BackendError::new("start", "boom").with_cleanup(vec!["runc delete: EBUSY".into()]);
        let shown = e.to_string();
        assert!(shown.contains("[start] boom"));
        assert!(shown.contains("1 cleanup errors"));
    }
}
