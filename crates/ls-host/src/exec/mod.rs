//! T10 — foreground execution, idempotent identity, real exit truth
//! (plans/tasks/T10.md; docs/v5/03_DOMAIN.md §命令与退出, §幂等与超时;
//! docs/v5/08_FAILURES.md §退出证据).
//!
//! Boundary laws honored here:
//! * RAW BYTES, NOT STRINGS: stdout/stderr are captured as `Vec<u8>` with a
//!   hard per-stream cap + truncation flag. The T08 backend String surface
//!   (`from_utf8_lossy`) is byte-lossy and is never consulted on this path.
//! * EXIT TRUTH COMES FROM A REAL WAIT PATH: the injected `lsguest-init`
//!   (T08) is reused as the exec WITNESS — `runc exec <ctr> /.lsbx/init
//!   --sandbox-id <id> --receipt <abs-guest-path> -- <argv...>`. POSIX
//!   128+n and `exit(128+n)` are indistinguishable at the runc CLI level;
//!   the witness receipt (kernel waitpid2 inside the guest) is what
//!   separates Exited from Signaled. The runc CLI code is cross-checked
//!   against the receipt — disagreement is an internal protocol error,
//!   never silently resolved.
//! * NO TEXT GUESSING: a missing/unparseable/foreign receipt degrades to
//!   `WitnessLost` → domain `unknown`, exactly like the T08 timeout law.
//!   `unknown` is never re-executed (domain Execution enforces).
//! * IDENTITY FIRST: every accepted request is urn-registered UNDER the
//!   idempotency scope BEFORE the effect runs; a same-scope same-digest
//!   replay returns the SAME operation and re-runs nothing; digest mismatch
//!   is CONFLICT. Rejected (never accepted) requests mint no operation and
//!   replay to the same rejection.
//! * ROOTLESS USER HONESTY: the PORTABLE_ROOTLESS user namespace maps one
//!   uid. A non-root `user` request is refused up front with
//!   PolicyUnsupported naming the mapped identity — never faked.

use ls_core::domain::access::SandboxRecord;
use ls_core::domain::error::{DomainError, ErrorCode};
use ls_core::domain::execution::{Execution, ExitRecord, TerminationReason};
use ls_core::domain::idempotency::{
    decide, request_digest, IdempotencyRecord, IdempotencyScope, Method, ReplayDecision,
};
use ls_core::domain::ids::{ExecutionId, OwnerId, SandboxId};
use ls_core::domain::lifecycle::ExecutionState;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::backend::InitReceiptTermination;
use crate::runtime::native::bundle::GUEST_INIT_PATH;
use crate::runtime::native::parse_receipt;

pub mod background;
use background::{BackgroundClock, BackgroundExecPort, BackgroundStart};

// ------------------------------------------------------------------ bounds

/// Per-stream capture bound. Anything beyond it is drained (so the guest
/// never deadlocks on a full pipe) and flagged, never kept.
pub const MAX_CAPTURE_BYTES: usize = 128 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_TIMEOUT_MS: u64 = 3_600_000;
pub const EXEC_API_VERSION: &str = "v1";
/// Canonical route of the exec creation verb (scope universe component).
pub const EXEC_ROUTE: &str = "/v1/executions";

// ------------------------------------------------------------------- spec

/// One materialized execution request. argv is explicit and non-empty: the
/// v1 front never guesses a shell (the wire `shell` kind is materialized to
/// `[sh, -c, script]` by the API layer BEFORE reaching here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    /// exec-local overlays over the container config env, KEY/VALUE pairs.
    pub env: Vec<(String, String)>,
    /// absolute guest path; "/" when unspecified.
    pub cwd: String,
    pub timeout_ms: u64,
    /// resolved (uid, gid) requested for the exec'd process; None = container
    /// default (root). Resolution from the wire string happens API-side.
    pub user: Option<(u32, u32)>,
}

impl ExecSpec {
    pub fn new(argv: Vec<String>) -> Self {
        ExecSpec {
            argv,
            env: Vec::new(),
            cwd: "/".to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            user: None,
        }
    }
}

/// Request-shape validation ONLY (no capability judgment). Failures are
/// InvalidArgument against the caller's body, never minted operations.
pub fn validate_exec_spec(spec: &ExecSpec) -> Result<(), DomainError> {
    if spec.argv.is_empty() {
        return Err(DomainError::invalid("exec argv must be non-empty"));
    }
    if spec.argv[0].is_empty() {
        return Err(DomainError::invalid("exec argv[0] must be non-empty"));
    }
    for (i, a) in spec.argv.iter().enumerate() {
        if a.contains('\0') {
            return Err(DomainError::invalid(format!("exec argv[{i}] contains NUL")));
        }
    }
    if !spec.cwd.starts_with('/') {
        return Err(DomainError::invalid("exec cwd must be an absolute guest path"));
    }
    if spec.cwd.contains('\0') {
        return Err(DomainError::invalid("exec cwd contains NUL"));
    }
    for (k, v) in &spec.env {
        if k.is_empty() || k.contains('=') || k.contains('\0') {
            return Err(DomainError::invalid(
                "exec env keys must be non-empty without '=' or NUL",
            ));
        }
        if v.contains('\0') {
            return Err(DomainError::invalid(format!("exec env {k} value contains NUL")));
        }
    }
    if spec.timeout_ms == 0 || spec.timeout_ms > MAX_TIMEOUT_MS {
        return Err(DomainError::invalid(format!(
            "exec timeout_ms must be 1..={MAX_TIMEOUT_MS}"
        )));
    }
    Ok(())
}

/// Canonical request body feeding the idempotency digest. Byte-order of the
/// *wire* payload is irrelevant; only the normalized execution request is
/// fingerprinted (env order-insensitive via BTreeMap → canonical_json's
/// sorted keys; the digest itself is the T04-tested
/// `idempotency::request_digest`).
pub fn canonical_body(spec: &ExecSpec) -> serde_json::Value {
    let env: serde_json::Map<String, serde_json::Value> = spec
        .env
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({
        "argv": spec.argv,
        "cwd": spec.cwd,
        "env": env,
        "timeout_ms": spec.timeout_ms,
        "user": spec.user.map(|(u, g)| json!([u, g])),
    })
}

// --------------------------------------------------------------- instance

/// A live container the exec attaches to (obtained from a real create —
/// never constructed from a self-claim).
#[derive(Debug, Clone)]
pub struct ActiveInstance {
    /// kernel/backend container label (e.g. `lsbx-sbx_…-g1`).
    pub container_id: String,
    pub sandbox_id: SandboxId,
    /// host path of the lease rootfs (witness receipts are read back here).
    pub rootfs: PathBuf,
    /// Host fact: the user namespace maps exactly one uid (PORTABLE_ROOTLESS).
    pub single_id_userns: bool,
}

// ---------------------------------------------------------------- outcome

/// Where a SpawnFailed testimony comes from. Both are real observations:
/// the witness receipt (guest wait path) or a host-side rootfs precheck
/// (fs metadata) — never a guess from stderr text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruthSource {
    WitnessReceipt,
    Precheck,
}

/// The mutually-exclusive exit truth of one exec, observation-carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitTruth {
    Exited(i32),
    Signaled(u32),
    /// The payload never ran (missing binary / exec permission / readonly
    /// relative-path resolution failure in the witness).
    SpawnFailed { detail: String, source: TruthSource },
    /// Frontend timeout: the process group was SIGKILLed; the exit result
    /// is lost BY DESIGN (never re-executed).
    TimedOut { after_ms: u64 },
    /// runc returned but the witness receipt is missing/unparseable/foreign:
    /// no wait-path evidence exists → unknown by law.
    WitnessLost { cli_exit: Option<i32> },
}

/// Bounded raw capture of one stream direction pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Everything one foreground exec observed.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub truth: ExitTruth,
    pub output: ExecOutput,
    pub wall: Duration,
    /// exit code of the `runc exec` CLI itself (None if the CLI was killed).
    /// Diagnostic cross-check material; the receipt stays authoritative.
    pub cli_exit: Option<i32>,
    /// verbatim witness receipt body (archive; None when lost).
    pub receipt_text: Option<String>,
}

// ------------------------------------------------------------------- port

/// Synchronous foreground executor. The T43 supervisor owns durability/
/// re-attach; this port is deliberately foreground-only (T10 scope).
pub trait ExecPort: Send + Sync {
    fn exec(
        &self,
        exec_id: &str,
        instance: &ActiveInstance,
        spec: &ExecSpec,
    ) -> Result<ExecOutcome, DomainError>;
}

// ------------------------------------------------------- runc witness port

/// Rootless-runc exec port that PREPENDS the T08 tiny init as witness.
///
/// Mirrors `runc.rs::exec_command` byte-for-byte on the shared segment
/// (`--root <sr> exec [--cwd c] [--env kv]... <id> <argv...>`, env_clear +
/// PATH/HOME/XDG_RUNTIME_DIR, identity-setuid pre_exec). `RuncRuntime`'s
/// caller-facing builder cannot carry the witness argv prefix + is in an
/// out-of-scope file, so this module builds its own Command — drift is
/// guarded by an equality unit test against `exec_command`'s output.
pub struct RuncWitnessExecPort {
    binary: PathBuf,
    state_root: PathBuf,
    identity: Option<(u32, u32)>,
    home: PathBuf,
}

mod sys {
    extern "C" {
        pub fn setuid(uid: u32) -> i32;
        pub fn setgid(gid: u32) -> i32;
        pub fn setpgid(pid: i32, pgid: i32) -> i32;
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

impl RuncWitnessExecPort {
    pub fn new(
        binary: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
        identity: Option<(u32, u32)>,
        home: impl Into<PathBuf>,
    ) -> Self {
        RuncWitnessExecPort {
            binary: binary.into(),
            state_root: state_root.into(),
            identity,
            home: home.into(),
        }
    }

    /// Guest-side witness receipt path (the init creates parent dirs itself).
    pub fn guest_receipt_path(exec_id: &str) -> String {
        format!("/.lsbx/exec/{exec_id}.receipt.json")
    }

    /// Host-side read-back path of the same receipt inside the lease rootfs.
    pub fn host_receipt_path(rootfs: &Path, exec_id: &str) -> PathBuf {
        rootfs
            .join(".lsbx")
            .join("exec")
            .join(format!("{exec_id}.receipt.json"))
    }

    /// The witness argv as the guest sees it. Protocol-v2 flags (`extra`)
    /// land BEFORE the `--` separator; the foreground path passes none, so
    /// the T10 wire stays byte-identical.
    fn witness_argv_with(
        instance: &ActiveInstance,
        spec: &ExecSpec,
        exec_id: &str,
        extra: &[String],
    ) -> Vec<String> {
        let mut v = vec![
            GUEST_INIT_PATH.to_string(),
            "--sandbox-id".to_string(),
            instance.sandbox_id.as_str().to_string(),
            "--receipt".to_string(),
            Self::guest_receipt_path(exec_id),
        ];
        v.extend(extra.iter().cloned());
        v.push("--".to_string());
        v.extend(spec.argv.iter().cloned());
        v
    }

    /// The witness argv as the guest sees it (T10 foreground: no extra flags).
    /// Used only by the argv-drift regression tests.
    #[cfg(test)]
    fn witness_argv(instance: &ActiveInstance, spec: &ExecSpec, exec_id: &str) -> Vec<String> {
        Self::witness_argv_with(instance, spec, exec_id, &[])
    }

    /// The shared segment builder — the drift-guarded mirror. Exposed as one
    /// fn so the unit test can assert arg-order equality with
    /// `RuncRuntime::exec_command` on identical inputs.
    fn build_command(
        &self,
        instance: &ActiveInstance,
        spec: &ExecSpec,
        exec_id: &str,
    ) -> Command {
        self.build_command_with(instance, spec, exec_id, &[])
    }

    /// The parameterized builder used by the background (protocol-v2) path.
    fn build_command_with(
        &self,
        instance: &ActiveInstance,
        spec: &ExecSpec,
        exec_id: &str,
        extra: &[String],
    ) -> Command {
        let guest = Self::witness_argv_with(instance, spec, exec_id, extra);
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--root")
            .arg(&self.state_root)
            .arg("exec")
            .arg("--cwd")
            .arg(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        cmd.arg(&instance.container_id).args(&guest);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.home)
            .stdin(Stdio::null());
        // Own process group FIRST, unconditionally (review F-01): a
        // frontend timeout must be able to SIGKILL the runc CLI AND any
        // in-container descendants it spawned (`kill -pgid`), in every
        // identity profile — gating this on `identity.is_some()` silently
        // broke the timeout kill-switch whenever no drop-identity was set.
        unsafe {
            let identity = self.identity;
            cmd.pre_exec(move || {
                if sys::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some((uid, gid)) = identity {
                    if sys::setgid(gid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if sys::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        cmd
    }

    /// Capture one pipe into a bounded Vec; drains overflow (no deadlock).
    fn capture_pipe<R: Read + Send + 'static>(
        mut r: R,
    ) -> thread::JoinHandle<io::Result<(Vec<u8>, bool)>> {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut truncated = false;
            loop {
                match r.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = MAX_CAPTURE_BYTES.saturating_sub(buf.len());
                        if room > 0 {
                            buf.extend_from_slice(&chunk[..n.min(room)]);
                        }
                        if n > room {
                            truncated = true;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok((buf, truncated))
        })
    }

    /// Host-side argv[0] precheck (absolute paths only). A readable
    /// testimony: missing/execute-bit/directory are SpawnFailed evidence
    /// from fs metadata — relative names are deliberately left to the
    /// witness (PATH resolution is guest-semantics).
    fn precheck(rootfs: &Path, argv0: &str) -> Option<String> {
        if !argv0.starts_with('/') {
            return None;
        }
        let host = rootfs.join(argv0.trim_start_matches('/'));
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(&host) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Some(format!("precheck: {argv0}: no such file in rootfs (ENOENT)"))
            }
            Err(e) => Some(format!("precheck: {argv0}: stat failed: {e}")),
            Ok(m) if m.is_dir() => Some(format!("precheck: {argv0}: is a directory")),
            Ok(m) if m.mode() & 0o111 == 0 => Some(format!(
                "precheck: {argv0}: lacks any execute bit (mode {:04o})",
                m.mode() & 0o7777
            )),
            Ok(_) => None,
        }
    }

    /// Read + strictly parse + ownership-check the witness receipt.
    /// Returns Ok(None) when no trustworthy receipt exists.
    fn read_receipt(
        &self,
        instance: &ActiveInstance,
        exec_id: &str,
    ) -> Result<Option<(String, crate::runtime::backend::InitReceipt)>, DomainError> {
        let path = Self::host_receipt_path(&instance.rootfs, exec_id);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(DomainError::new(
                    ErrorCode::InternalFailure,
                    format!("witness receipt read {}: {e}", path.display()),
                ))
            }
        };
        match parse_receipt(&text) {
            Some(r) if r.sandbox_id == instance.sandbox_id.as_str() => Ok(Some((text, r))),
            // parse failure OR foreign sandbox_id: never trust it.
            _ => Ok(None),
        }
    }

    /// Receipt → ExitTruth with the CLI cross-check (T10 law, verbatim).
    /// Shared by the foreground tail and the T11 background finish path.
    fn truth_from_receipt(
        &self,
        cli_exit: Option<i32>,
        receipt: &crate::runtime::backend::InitReceipt,
        receipt_text: &str,
    ) -> Result<ExitTruth, DomainError> {
        let expected: i32 = match &receipt.termination {
            InitReceiptTermination::Exited { exit_code } => (exit_code & 0xff) as i32,
            InitReceiptTermination::Signaled { signal } => 128 + (signal & 0x7f) as i32,
            InitReceiptTermination::SpawnFailed { .. } => 127,
            InitReceiptTermination::WaitInvariantBroken { .. } => {
                return Err(DomainError::new(
                    ErrorCode::InternalFailure,
                    "witness receipt reports a broken wait invariant",
                ))
            }
        };
        if let Some(c) = cli_exit {
            if c != expected {
                return Err(DomainError::new(
                    ErrorCode::InternalFailure,
                    format!(
                        "witness/CLI disagreement: runc exit {c} but receipt implies {expected} (receipt: {receipt_text})"
                    ),
                ));
            }
        }
        Ok(match &receipt.termination {
            InitReceiptTermination::Exited { exit_code } => ExitTruth::Exited(*exit_code as i32),
            InitReceiptTermination::Signaled { signal } => ExitTruth::Signaled(*signal as u32),
            InitReceiptTermination::SpawnFailed { detail } => ExitTruth::SpawnFailed {
                detail: detail.clone(),
                source: TruthSource::WitnessReceipt,
            },
            InitReceiptTermination::WaitInvariantBroken { .. } => unreachable!(),
        })
    }
}

impl ExecPort for RuncWitnessExecPort {
    fn exec(
        &self,
        exec_id: &str,
        instance: &ActiveInstance,
        spec: &ExecSpec,
    ) -> Result<ExecOutcome, DomainError> {
        // 1) host-side precheck: absolute argv[0] missing/no-exec is a
        //    SpawnFailed truth from fs metadata (still NOT text guessing).
        if let Some(detail) = Self::precheck(&instance.rootfs, &spec.argv[0]) {
            return Ok(ExecOutcome {
                truth: ExitTruth::SpawnFailed {
                    detail,
                    source: TruthSource::Precheck,
                },
                output: ExecOutput::default(),
                wall: Duration::from_millis(0),
                cli_exit: None,
                receipt_text: None,
            });
        }

        // 2) spawn the witness chain.
        let mut cmd = self.build_command(instance, spec, exec_id);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            DomainError::new(
                ErrorCode::InternalFailure,
                format!("runc exec spawn: {e}"),
            )
        })?;
        let pid = child.id() as i32;
        let stdout_t = Self::capture_pipe(child.stdout.take().expect("piped"));
        let stderr_t = Self::capture_pipe(child.stderr.take().expect("piped"));

        // 3) wait with a hard deadline; on expiry SIGKILL the whole group.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        let deadline = started + Duration::from_millis(spec.timeout_ms);
        let mut timed_out = false;
        let cli_status: Option<std::process::ExitStatus> = loop {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                unsafe {
                    let _ = sys::kill(-pid, 9);
                }
                // bounded grace to reap the killed CLI
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(Ok(s)) => break Some(s),
                    _ => break None,
                }
            }
            let step = (deadline - now).min(Duration::from_millis(50));
            match rx.recv_timeout(step) {
                Ok(Ok(s)) => break Some(s),
                Ok(Err(e)) => {
                    return Err(DomainError::new(
                        ErrorCode::InternalFailure,
                        format!("runc exec wait: {e}"),
                    ))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(DomainError::new(
                        ErrorCode::InternalFailure,
                        "runc exec waiter thread vanished",
                    ))
                }
            }
        };
        let wall = started.elapsed();

        // 4) join capture threads (pipes hit EOF when the group died).
        let join = |h: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>| match h.join() {
            Ok(Ok(v)) => Ok(v),
            _ => Err(DomainError::new(
                ErrorCode::InternalFailure,
                "exec capture thread failed",
            )),
        };
        let (stdout, stdout_truncated) = join(stdout_t)?;
        let (stderr, stderr_truncated) = join(stderr_t)?;
        let output = ExecOutput {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        };
        let cli_exit = cli_status.and_then(|s| s.code());

        // 5) classify: timeout law first; then receipt-or-WitnessLost.
        if timed_out {
            return Ok(ExecOutcome {
                truth: ExitTruth::TimedOut {
                    after_ms: spec.timeout_ms,
                },
                output,
                wall,
                cli_exit,
                receipt_text: None,
            });
        }

        let Some((receipt_text, receipt)) = self.read_receipt(instance, exec_id)? else {
            // A live CLI exit code WITHOUT wait-path evidence is not an exit
            // truth (128+n ambiguity). unknown by law.
            return Ok(ExecOutcome {
                truth: ExitTruth::WitnessLost { cli_exit },
                output,
                wall,
                cli_exit,
                receipt_text: None,
            });
        };

        // 6) cross-check CLI code against receipt-implied init code.
        let truth = self.truth_from_receipt(cli_exit, &receipt, &receipt_text)?;
        Ok(ExecOutcome {
            truth,
            output,
            wall,
            cli_exit,
            receipt_text: Some(receipt_text),
        })
    }
}

// ---------------------------------------------------------------- service

/// Immutable public snapshot of one execution for start/inspect views.
#[derive(Debug, Clone)]
pub struct ExecutionView {
    pub execution_id: ExecutionId,
    pub state: ExecutionState,
    pub exit_record: Option<ExitRecord>,
    pub termination: TerminationReason,
    pub audits: Vec<String>,
    pub replayed: bool,
    /// present iff the frontend itself hit an infra-level error (never a
    /// payload failure — those live in exit_record/start_failed).
    pub backend_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StartOutcome {
    pub view: ExecutionView,
    /// terminal observation snapshot; None while running (replay during
    /// flight) — the foreground caller almost always has it.
    pub outcome: Option<ExecOutcome>,
}

struct Registration {
    record: IdempotencyRecord,
    execution: Execution,
    outcome: Option<ExecOutcome>,
    backend_error: Option<String>,
    /// T11 background only: cancel-attribution flag shared with the
    /// supervising waiter thread. The witness receipt stays the exit truth;
    /// this flag only selects Cancelled vs natural attribution.
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// T11 background only: terminal-publication gate for `wait_execution`.
    bg_gate: Option<std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>>,
    /// T11 background only: host rootfs for the ctrl kill-switch write.
    ctrl_rootfs: Option<PathBuf>,
}

/// Identity-decision result of the shared front-door gate (T10 foreground
/// and T11 background use the identical idempotency law).
enum MintOutcome {
    Replay(StartOutcome),
    Fresh { key: String, exec_id: ExecutionId },
}

/// What kind of registration the gate mints. Foreground registrations carry
/// no T11 extras; the wire-visible behavior is unchanged.
enum MintMode {
    Foreground,
    Background {
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        gate: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
        rootfs: PathBuf,
    },
}

#[derive(Default)]
struct Registry {
    by_scope: HashMap<String, Registration>,
    by_exec: HashMap<String, String>,
}

/// Foreground exec front: identity + idempotency + domain state machine
/// over an ExecPort. All mutating paths funnel through one lock; the port
/// runs OUTSIDE the lock so replays inspect the placeholder (starting)
/// rather than double-firing the effect.
pub struct ExecService<P: ExecPort> {
    port: P,
    /// Arc-shared so a T11 supervising waiter thread can publish terminals
    /// after the request thread returned. All mutating paths still funnel
    /// through this one lock.
    reg: std::sync::Arc<Mutex<Registry>>,
    seq: std::sync::atomic::AtomicU64,
}

impl<P: ExecPort> ExecService<P> {
    pub fn new(port: P) -> Self {
        ExecService {
            port,
            reg: std::sync::Arc::new(Mutex::new(Registry::default())),
            seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn port(&self) -> &P {
        &self.port
    }

    fn mint_exec_id(&self) -> Result<ExecutionId, DomainError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| DomainError::new(ErrorCode::InternalFailure, format!("clock: {e}")))?
            .as_nanos();
        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ExecutionId::new(format!("exec-{nanos}-{n}"))
    }

    /// The scope-universe separator is forbidden inside scope fields at
    /// this layer (ls-core only bans NUL) so the serialized map key is
    /// collision-free by construction.
    fn scope_key(scope: &IdempotencyScope) -> Result<String, DomainError> {
        for f in [
            scope.principal.as_str(),
            &scope.api_version,
            &scope.canonical_route,
            &scope.resource_identity,
            &scope.key,
        ] {
            if f.contains('\u{1f}') {
                return Err(DomainError::invalid(
                    "idempotency scope fields must not contain U+001F",
                ));
            }
        }
        Ok(format!(
            "{}\u{1f}{}\u{1f}{:?}\u{1f}{}\u{1f}{}\u{1f}{}",
            scope.principal.as_str(),
            scope.api_version,
            scope.method,
            scope.canonical_route,
            scope.resource_identity,
            scope.key
        ))
    }

    fn view_of(r: &Registration, replayed: bool) -> ExecutionView {
        ExecutionView {
            execution_id: r.execution.id().clone(),
            state: r.execution.state(),
            exit_record: r.execution.exit_record(),
            termination: r.execution.termination(),
            audits: r.execution.audits().to_vec(),
            replayed,
            backend_error: r.backend_error.clone(),
        }
    }

    /// Shared identity gate for both execution doors (T10 foreground, T11
    /// background): request-shape validation, capability honesty, scope +
    /// digest from the CANONICAL body, and the one-lock identity decision.
    /// A fresh mint plants the placeholder BEFORE any effect, so a
    /// concurrent replay observes `starting` and can never double-fire.
    fn gate_and_mint(
        &self,
        principal: &OwnerId,
        instance: &ActiveInstance,
        operation_key: &str,
        spec: &ExecSpec,
        mode: MintMode,
    ) -> Result<MintOutcome, DomainError> {
        // 1) request-shape validation — before any identity exists.
        validate_exec_spec(spec)?;

        // 2) capability honesty: a rootless single-id userns cannot honor a
        //    non-root exec user. Refuse BEFORE minting; replays refuse the
        //    same way (no operation is ever created).
        if instance.single_id_userns {
            if let Some((u, g)) = spec.user {
                if (u, g) != (0, 0) {
                    return Err(DomainError::new(
                        ErrorCode::PolicyUnsupported,
                        format!(
                            "exec user {u}:{g} cannot be honored: this rootless user namespace maps exactly one uid/gid pair (PORTABLE_ROOTLESS); only 0:0 is resolvable"
                        ),
                    ));
                }
            }
        }

        // 3) scope + digest from the CANONICAL body (never from wire bytes).
        let scope = IdempotencyScope::new(
            principal.clone(),
            EXEC_API_VERSION,
            Method::Post,
            EXEC_ROUTE,
            instance.sandbox_id.as_str(),
            operation_key,
        )?;
        let digest = request_digest(&canonical_body(spec))?;
        let key = Self::scope_key(&scope)?;

        // 4) identity decision under the single lock.
        let mut reg = self.reg.lock().expect("exec registry poisoned");
        let existing = reg.by_scope.get(&key).map(|r| &r.record);
        match decide(existing, &scope, &digest)? {
            ReplayDecision::Replay { result_ref } => {
                let r = reg.by_scope.get(&key).expect("replay row");
                debug_assert_eq!(r.execution.id().as_str(), result_ref);
                Ok(MintOutcome::Replay(StartOutcome {
                    view: Self::view_of(r, true),
                    outcome: r.outcome.clone(),
                }))
            }
            ReplayDecision::Fresh => {
                let exec_id = self.mint_exec_id()?;
                // binding-only record: the Fencing/Manager binding lives
                // in the lifecycle manager; Execution requires one.
                let rec = SandboxRecord::active(instance.sandbox_id.clone(), principal.clone());
                let binding =
                    ls_core::domain::access::ExecutionBinding::new(exec_id.clone(), &rec);
                let mut execution = Execution::new(exec_id.clone(), binding);
                execution.transition(ExecutionState::Starting)?;
                let (cancel, bg_gate, ctrl_rootfs) = match mode {
                    MintMode::Foreground => (None, None, None),
                    MintMode::Background {
                        cancel,
                        gate,
                        rootfs,
                    } => (Some(cancel), Some(gate), Some(rootfs)),
                };
                reg.by_scope.insert(
                    key.clone(),
                    Registration {
                        record: IdempotencyRecord {
                            scope: scope.clone(),
                            request_digest: digest.clone(),
                            result_ref: exec_id.as_str().to_string(),
                        },
                        execution,
                        outcome: None,
                        backend_error: None,
                        cancel,
                        bg_gate,
                        ctrl_rootfs,
                    },
                );
                reg.by_exec.insert(exec_id.as_str().to_string(), key.clone());
                Ok(MintOutcome::Fresh { key, exec_id })
            }
        }
    }

    /// Fold observed truth into the domain state machine; park the outcome.
    /// Domain errors here are internal by construction (the placeholder is
    /// always `starting` at this point). This is the T10 law, verbatim, and
    /// the foreground door's only fold path.
    fn fold_outcome(
        r: &mut Registration,
        effect: Result<ExecOutcome, DomainError>,
    ) -> Result<(), DomainError> {
        match effect {
            Ok(outcome) => {
                match &outcome.truth {
                    ExitTruth::Exited(code) => {
                        r.execution.transition(ExecutionState::Running)?;
                        r.execution.finish_exited(*code)?;
                    }
                    ExitTruth::Signaled(sig) => {
                        r.execution.transition(ExecutionState::Running)?;
                        r.execution.finish_signaled(*sig)?;
                    }
                    ExitTruth::SpawnFailed { .. } => {
                        // The detail + source live on the parked ExecOutcome
                        // (truth-carried evidence), never in free-text audits.
                        r.execution.fail_start()?;
                    }
                    ExitTruth::TimedOut { .. } => {
                        r.execution.transition(ExecutionState::Running)?;
                        // mark BEFORE terminal: the independent termination
                        // dimension refuses marks on terminal executions.
                        r.execution.mark_termination(TerminationReason::Timeout)?;
                        r.execution.finish_unknown()?;
                    }
                    ExitTruth::WitnessLost { .. } => {
                        r.execution.transition(ExecutionState::Running)?;
                        r.execution.finish_unknown()?;
                    }
                }
                r.outcome = Some(outcome);
            }
            Err(e) => {
                r.execution.fail_start()?;
                r.backend_error = Some(format!("{e}"));
            }
        }
        Ok(())
    }

    /// The one front door (spec §幂等与超时).
    pub fn start_execution(
        &self,
        principal: &OwnerId,
        instance: &ActiveInstance,
        operation_key: &str,
        spec: ExecSpec,
    ) -> Result<StartOutcome, DomainError> {
        let (key, exec_id) =
            match self.gate_and_mint(principal, instance, operation_key, &spec, MintMode::Foreground)? {
                MintOutcome::Replay(outcome) => return Ok(outcome),
                MintOutcome::Fresh { key, exec_id } => (key, exec_id),
            };

        // 5) the effect — strictly outside the lock.
        let effect: Result<ExecOutcome, DomainError> =
            self.port.exec(exec_id.as_str(), instance, &spec);

        // 6) fold observed truth into the domain state machine; park the outcome.
        let mut reg = self.reg.lock().expect("exec registry poisoned");
        let r = reg.by_scope.get_mut(&key).expect("fresh row");
        Self::fold_outcome(r, effect)?;
        Ok(StartOutcome {
            view: Self::view_of(r, false),
            outcome: r.outcome.clone(),
        })
    }

    /// T11 — cancel a background execution. Response law (card C04 +
    /// spec semantics table):
    /// * unknown id / foreign principal                → NotFound
    /// * a FOREGROUND execution                        → Conflict (its only
    ///   horizon is the caller's own timeout)
    /// * already terminal, attributed Cancelled        → Ok (idempotent)
    /// * already terminal, any other outcome           → Conflict (a cancel
    ///   request must never overwrite a terminal record)
    /// * cancel already requested, still in flight     → Ok (idempotent)
    /// * fresh cancel while running                    → flag + kill-switch
    ///   write; the process continues until observed terminal by the
    ///   supervising waiter thread.
    ///
    /// Cancel never *declares* a kill: it arms the attribution flag and the
    /// witness's kill switch. The exit record still comes from the real
    /// waitpid path; a lost witness degrades to unknown, not to a faked
    /// exit. A cancel write failure is logged (stderr), and the background
    /// budget horizon still bounds the execution.
    pub fn cancel_execution(
        &self,
        principal: &OwnerId,
        execution_id: &ExecutionId,
    ) -> Result<StartOutcome, DomainError> {
        let mut reg = self.reg.lock().expect("exec registry poisoned");
        let Some(key) = reg.by_exec.get(execution_id.as_str()).cloned() else {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                format!("execution {execution_id:?} not found"),
            ));
        };
        let r = reg.by_scope.get_mut(&key).expect("by_exec backlink");
        if r.record.scope.principal != *principal {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                format!("execution {execution_id:?} not found"),
            ));
        }
        if r.bg_gate.is_none() {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "foreground executions are not separately cancellable: their horizon is the caller-side timeout",
            ));
        }
        if r.execution.state().is_terminal() {
            if r.execution.termination() == TerminationReason::Cancelled {
                // idempotent repeated cancel of a cancelled execution
                return Ok(StartOutcome {
                    view: Self::view_of(r, true),
                    outcome: r.outcome.clone(),
                });
            }
            return Err(DomainError::new(
                ErrorCode::Conflict,
                format!(
                    "execution {} is already terminal; a late cancel cannot overwrite the observed result",
                    execution_id.as_str()
                ),
            ));
        }
        let flag = r.cancel.clone().expect("bg registration has cancel flag");
        if flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
            // cancel already armed; kill switch already (attempted) written.
            return Ok(StartOutcome {
                view: Self::view_of(r, true),
                outcome: r.outcome.clone(),
            });
        }
        if let Some(rootfs) = &r.ctrl_rootfs {
            background::fire_cancel(&background::host_ctrl_path(
                rootfs,
                execution_id.as_str(),
            ));
        }
        Ok(StartOutcome {
            view: Self::view_of(r, true),
            outcome: r.outcome.clone(),
        })
    }

    /// T11 — block until the execution is terminal or `budget` expires.
    /// Expiry is NOT an error and NOT a cancellation: the caller simply gets
    /// the still-running view. Unknown ids stay NotFound. Foreground
    /// executions are terminal by the time they are visible, so their view
    /// returns immediately.
    pub fn wait_execution(
        &self,
        principal: &OwnerId,
        execution_id: &ExecutionId,
        budget: Duration,
    ) -> Result<StartOutcome, DomainError> {
        let deadline = Instant::now() + budget;
        loop {
            let (gate, current) = {
                let reg = self.reg.lock().expect("exec registry poisoned");
                let Some(key) = reg.by_exec.get(execution_id.as_str()) else {
                    return Err(DomainError::new(
                        ErrorCode::NotFound,
                        format!("execution {execution_id:?} not found"),
                    ));
                };
                let r = reg.by_scope.get(key).expect("by_exec backlink");
                if r.record.scope.principal != *principal {
                    return Err(DomainError::new(
                        ErrorCode::NotFound,
                        format!("execution {execution_id:?} not found"),
                    ));
                }
                if r.execution.state().is_terminal() {
                    return Ok(StartOutcome {
                        view: Self::view_of(r, false),
                        outcome: r.outcome.clone(),
                    });
                }
                let Some(gate) = r.bg_gate.clone() else {
                    // foreground placeholder still folding; spin briefly via
                    // the same gate-less path by returning the live view.
                    return Ok(StartOutcome {
                        view: Self::view_of(r, false),
                        outcome: r.outcome.clone(),
                    });
                };
                (gate, ())
            };
            let _ = current;
            let (m, cv) = &*gate;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let reg = self.reg.lock().expect("exec registry poisoned");
                let key = reg.by_exec.get(execution_id.as_str()).expect("still registered");
                let r = reg.by_scope.get(key).expect("by_exec backlink");
                return Ok(StartOutcome {
                    view: Self::view_of(r, false),
                    outcome: r.outcome.clone(),
                });
            }
            let mut published = m.lock().expect("bg gate poisoned");
            while !*published {
                let step = deadline.saturating_duration_since(Instant::now());
                if step.is_zero() {
                    break;
                }
                let (guard, _) = cv
                    .wait_timeout(published, step)
                    .expect("bg gate poisoned");
                published = guard;
            }
            drop(published);
            // publish_background folds the terminal under the registry lock
            // BEFORE setting the gate flag; re-entering the loop now always
            // observes the terminal state (or the budget finally expired).
        }
    }

    /// Inspect by execution identity. A principal mismatch is deliberately
    /// indistinguishable from absence (no existence probing).
    pub fn get_execution(
        &self,
        principal: &OwnerId,
        execution_id: &ExecutionId,
    ) -> Result<StartOutcome, DomainError> {
        let reg = self.reg.lock().expect("exec registry poisoned");
        let Some(key) = reg.by_exec.get(execution_id.as_str()) else {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                format!("execution {execution_id:?} not found"),
            ));
        };
        let r = reg.by_scope.get(key).expect("by_exec backlink");
        if r.record.scope.principal != *principal {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                format!("execution {execution_id:?} not found"),
            ));
        }
        Ok(StartOutcome {
            view: Self::view_of(r, false),
            outcome: r.outcome.clone(),
        })
    }
}

// ------------------------------------------------------- background (T11)

impl<P: BackgroundExecPort + 'static> ExecService<P> {
    /// T11 background door: register under the SAME idempotency law, spawn
    /// the supervised witness chain (nested pidns + ctrl kill-switch), and
    /// hand reaping to a supervising waiter thread. Returns as soon as the
    /// chain is up (Running) with `outcome: None`; the terminal is observed
    /// via `wait_execution` / `get_execution` / `cancel_execution`.
    ///
    /// `budget` is the background horizon that replaces the caller-side
    /// foreground timeout as the kill deadline (capped at MAX_TIMEOUT_MS).
    /// Cancellation mid-flight is expressed via `cancel_execution`.
    pub fn start_background_execution(
        self: &std::sync::Arc<Self>,
        principal: &OwnerId,
        instance: &ActiveInstance,
        operation_key: &str,
        spec: ExecSpec,
        budget: Duration,
    ) -> Result<StartOutcome, DomainError> {
        if budget.is_zero() || budget > Duration::from_millis(MAX_TIMEOUT_MS) {
            return Err(DomainError::invalid(format!(
                "background budget must be 1ms..={MAX_TIMEOUT_MS}ms"
            )));
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (key, exec_id) = match self.gate_and_mint(
            principal,
            instance,
            operation_key,
            &spec,
            MintMode::Background {
                cancel: cancel.clone(),
                gate: gate.clone(),
                rootfs: instance.rootfs.clone(),
            },
        )? {
            MintOutcome::Replay(outcome) => return Ok(outcome),
            MintOutcome::Fresh { key, exec_id } => (key, exec_id),
        };

        // spawn the supervised chain — strictly outside the lock.
        match self.port.spawn_background(exec_id.as_str(), instance, &spec) {
            Err(e) => {
                // Infra-level spawn failure: fail_start + backend_error,
                // identical law to the foreground effect error.
                self.publish_background(&key, Err(e))?;
                let reg = self.reg.lock().expect("exec registry poisoned");
                let r = reg.by_scope.get(&key).expect("fresh row");
                Ok(StartOutcome {
                    view: Self::view_of(r, false),
                    outcome: r.outcome.clone(),
                })
            }
            Ok(BackgroundStart::PrecheckFailed(outcome)) => {
                // Host precheck truth: SpawnFailed, no chain ever existed.
                self.publish_background(&key, Ok((outcome, BackgroundClock::default())))?;
                let reg = self.reg.lock().expect("exec registry poisoned");
                let r = reg.by_scope.get(&key).expect("fresh row");
                Ok(StartOutcome {
                    view: Self::view_of(r, false),
                    outcome: r.outcome.clone(),
                })
            }
            Ok(BackgroundStart::Spawned(spawn)) => {
                {
                    let mut reg = self.reg.lock().expect("exec registry poisoned");
                    let r = reg.by_scope.get_mut(&key).expect("fresh row");
                    r.execution.transition(ExecutionState::Running)?;
                }
                let svc = std::sync::Arc::clone(self);
                let waiter_key = key.clone();
                let waiter_exec_id = exec_id.clone();
                let waiter_instance = instance.clone();
                thread::spawn(move || {
                    let effect = svc.port.finish_background(
                        spawn,
                        waiter_exec_id.as_str(),
                        &waiter_instance,
                        budget,
                        &cancel,
                    );
                    if let Err(e) = svc.publish_background(&waiter_key, effect) {
                        // An unpublishable terminal is an internal protocol
                        // break: log loudly; the registration stays
                        // non-terminal rather than lying.
                        eprintln!("ls-host: background publish failed for key {waiter_key}: {e}");
                    }
                });
                let reg = self.reg.lock().expect("exec registry poisoned");
                let r = reg.by_scope.get(&key).expect("fresh row");
                Ok(StartOutcome {
                    view: Self::view_of(r, false),
                    outcome: None,
                })
            }
        }
    }

    /// The supervising waiter thread's terminal publication: fold the
    /// observed truth (with T11 attribution), then open the wait gate.
    fn publish_background(
        &self,
        key: &str,
        effect: Result<(ExecOutcome, BackgroundClock), DomainError>,
    ) -> Result<(), DomainError> {
        let gate = {
            let mut reg = self.reg.lock().expect("exec registry poisoned");
            let r = reg.by_scope.get_mut(key).expect("background row");
            Self::fold_background_outcome(r, effect)?;
            r.bg_gate.clone()
        };
        if let Some(gate) = gate {
            let (m, cv) = &*gate;
            let mut published = m.lock().expect("bg gate poisoned");
            *published = true;
            cv.notify_all();
        }
        Ok(())
    }

    /// T11 attribution law (see exec::background module docs): the observed
    /// wait truth is folded EXACTLY as the foreground path folds it; the
    /// termination dimension is additionally marked from host-side facts:
    /// deadline fired → Timeout; cancel flag + kill-compatible truth
    /// (Signaled / lost evidence) → Cancelled; a natural observed exit that
    /// beat a racing cancel keeps NO termination mark. mark_termination's
    /// set-once law makes Timeout-after-Cancelled (or vice versa) a logged
    /// conflict on the ALREADY terminal record — never an overwrite.
    fn fold_background_outcome(
        r: &mut Registration,
        effect: Result<(ExecOutcome, BackgroundClock), DomainError>,
    ) -> Result<(), DomainError> {
        // The door transitions Starting→Running once the chain is spawned;
        // the fold may observe either state. Running-only truths are
        // promoted exactly once, mirroring the foreground fold law.
        let promote = |r: &mut Registration| -> Result<(), DomainError> {
            if r.execution.state() == ExecutionState::Starting {
                r.execution.transition(ExecutionState::Running)?;
            }
            Ok(())
        };
        let (outcome, clock) = match effect {
            Ok(pair) => pair,
            Err(e) => {
                // Infra-level wait failure: pre-spawn this is a start
                // failure; after spawn the evidence is lost → unknown. Both
                // terminal, neither fabricated.
                if r.execution.state() == ExecutionState::Starting {
                    r.execution.fail_start()?;
                } else {
                    r.execution.finish_unknown()?;
                }
                r.backend_error = Some(format!("{e}"));
                return Ok(());
            }
        };
        let cancel_hit = r
            .cancel
            .as_ref()
            .map(|f| background::cancel_requested(f))
            .unwrap_or(false);
        let attribution = if clock.deadline_fired {
            Some(TerminationReason::Timeout)
        } else if cancel_hit
            && matches!(
                &outcome.truth,
                ExitTruth::Signaled(_) | ExitTruth::WitnessLost { .. } | ExitTruth::TimedOut { .. }
            )
        {
            Some(TerminationReason::Cancelled)
        } else {
            None
        };
        let mark = |r: &mut Registration| {
            if let Some(reason) = attribution {
                if let Err(e) = r.execution.mark_termination(reason) {
                    eprintln!("ls-host: termination attribution conflict kept observed record: {e}");
                }
            }
        };
        match &outcome.truth {
            ExitTruth::Exited(code) => {
                promote(r)?;
                mark(r);
                r.execution.finish_exited(*code)?;
            }
            ExitTruth::Signaled(sig) => {
                promote(r)?;
                mark(r);
                r.execution.finish_signaled(*sig)?;
            }
            ExitTruth::SpawnFailed { detail, source } => {
                if r.execution.state() == ExecutionState::Starting {
                    r.execution.fail_start()?;
                } else {
                    // Frozen table law: running → start-failed is an illegal
                    // edge. A pre-payload failure observed AFTER the Running
                    // placeholder means the placeholder raced the evidence,
                    // not that a running payload vanished. Keep the observed
                    // truth in `outcome`, settle Unknown (the on-disk receipt
                    // is the reconcile_unknown path back to StartFailed), and
                    // log loudly. Never publish an illegal transition.
                    eprintln!(
                        "ls-host: SpawnFailed arrived after Running promote \
                         ({source:?}: {detail}); settling Unknown pending reconcile"
                    );
                    r.execution.finish_unknown()?;
                }
            }
            ExitTruth::TimedOut { .. } => {
                promote(r)?;
                mark(r);
                r.execution.finish_unknown()?;
            }
            ExitTruth::WitnessLost { .. } => {
                promote(r)?;
                mark(r);
                r.execution.finish_unknown()?;
            }
        }
        r.outcome = Some(outcome);
        Ok(())
    }
}

// --------------------------------------------------------- contract bridge

use ls_core::contracts::{Command as ContractCommand, ExecSpec as ContractExecSpec};

/// Materialize a frozen-contract exec spec into this module's host spec.
/// The `shell` kind becomes `[executable, "-c", script]` — always a `-c`
/// invocation, never a login shell (03_DOMAIN §命令与退出). The resolved
/// numeric user is supplied separately (see `resolve_user`).
pub fn spec_from_contract(
    cs: &ContractExecSpec,
    resolved_user: Option<(u32, u32)>,
) -> Result<ExecSpec, DomainError> {
    let argv = match &cs.command {
        ContractCommand::Argv(argv) => argv.clone(),
        ContractCommand::Shell { executable, script } => {
            vec![executable.clone(), "-c".to_string(), script.clone()]
        }
    };
    let mut spec = ExecSpec {
        argv,
        env: cs
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str()
                        .expect("contract parse pins env values to strings")
                        .to_string(),
                )
            })
            .collect(),
        cwd: cs.cwd.clone().unwrap_or_else(|| "/".to_string()),
        timeout_ms: cs.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
        user: resolved_user,
    };
    // env order must not leak into anything observable: keep BTreeMap order
    // (deterministic), matching canonical_body's sorted-key digest.
    spec.env.sort();
    validate_exec_spec(&spec)?;
    Ok(spec)
}

/// Resolve the wire `user` string to a numeric (uid, gid) pair *within the
/// rootfs semantics* (03_DOMAIN): names are looked up in the sandbox image's
/// own /etc/passwd and /etc/group, never in the host's.
///
/// Accepted forms:
/// * `"u[:g]"`      — numeric; a lone uid also selects gid=uid
/// * `"name"`       — /etc/passwd lookup supplies both uid and gid
/// * `"name:g"`     — passwd for the user + numeric gid
/// * `"name:group"` — passwd + /etc/group
///
/// `None` selects the container default (root). A missing user database is
/// only tolerated for the literal name "root", whose kernel meaning is fixed
/// at (0,0); every other name demands real rootfs evidence — never a guess.
pub fn resolve_user(user: Option<&str>, rootfs: &Path) -> Result<Option<(u32, u32)>, DomainError> {
    let Some(raw) = user else { return Ok(None) };
    let (uname, gname) = match raw.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (raw, None),
    };
    if uname.is_empty() {
        return Err(DomainError::invalid("exec user must not be empty"));
    }

    let uid_from = |name: &str| -> Result<u32, DomainError> {
        if let Ok(u) = name.parse::<u32>() {
            return Ok(u);
        }
        match lookup_passwd(rootfs, name)? {
            Some((u, _)) => Ok(u),
            None if name == "root" => Ok(0),
            None => Err(DomainError::new(
                ErrorCode::NotFound,
                format!("exec user {name:?} not resolvable: /etc/passwd lacks it in the sandbox rootfs"),
            )),
        }
    };

    let uid = uid_from(uname)?;
    let gid = match gname {
        None => {
            if uname.parse::<u32>().is_ok() {
                uid // lone numeric uid selects gid=uid
            } else if uname != "root" {
                lookup_passwd(rootfs, uname)?
                    .map(|(_, g)| g)
                    .ok_or_else(|| {
                        DomainError::new(
                            ErrorCode::NotFound,
                            format!("exec user {uname:?} not in the sandbox rootfs /etc/passwd"),
                        )
                    })?
            } else {
                match lookup_passwd(rootfs, "root")? {
                    Some((_, g)) => g,
                    None => 0,
                }
            }
        }
        Some(g) => {
            if let Ok(n) = g.parse::<u32>() {
                n
            } else {
                lookup_group(rootfs, g)?.ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::NotFound,
                        format!("exec group {g:?} not in the sandbox rootfs /etc/group"),
                    )
                })?
            }
        }
    };
    Ok(Some((uid, gid)))
}

/// `name:passwd:uid:gid:gecos:home:shell` — returns (uid, gid) for `name`.
fn lookup_passwd(rootfs: &Path, name: &str) -> Result<Option<(u32, u32)>, DomainError> {
    let p = rootfs.join("etc/passwd");
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(DomainError::new(
                ErrorCode::InternalFailure,
                format!("read {}: {e}", p.display()),
            ))
        }
    };
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 4 && f[0] == name {
            let uid = f[2].parse::<u32>().map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalFailure,
                    format!("malformed passwd uid in {}: {line}", p.display()),
                )
            })?;
            let gid = f[3].parse::<u32>().map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalFailure,
                    format!("malformed passwd gid in {}: {line}", p.display()),
                )
            })?;
            return Ok(Some((uid, gid)));
        }
    }
    Ok(None)
}

/// `name:passwd:gid:members` — returns gid for `name`.
fn lookup_group(rootfs: &Path, name: &str) -> Result<Option<u32>, DomainError> {
    let p = rootfs.join("etc/group");
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(DomainError::new(
                ErrorCode::InternalFailure,
                format!("read {}: {e}", p.display()),
            ))
        }
    };
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3 && f[0] == name {
            let gid = f[2].parse::<u32>().map_err(|_| {
                DomainError::new(
                    ErrorCode::InternalFailure,
                    format!("malformed group gid in {}: {line}", p.display()),
                )
            })?;
            return Ok(Some(gid));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::native::runc::RuncRuntime;
    use ls_core::domain::ids::SandboxId;
    use std::collections::VecDeque;
    use std::ffi::OsStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn owner() -> OwnerId {
        OwnerId::new("alice").unwrap()
    }

    fn tmp_rootfs(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ls-t10-exec-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn inst(rootfs: &Path) -> ActiveInstance {
        ActiveInstance {
            container_id: "lsbx-sbx-t10-g1".to_string(),
            sandbox_id: SandboxId::new("sbx-t10").unwrap(),
            rootfs: rootfs.to_path_buf(),
            single_id_userns: true,
        }
    }

    fn spec(argv: &[&str]) -> ExecSpec {
        ExecSpec::new(argv.iter().map(ToString::to_string).collect())
    }

    // --------------------------------------------------------- validation

    #[test]
    fn validation_rejects_malformed_requests() {
        let mut s = spec(&["/bin/echo", "hi"]);
        assert!(validate_exec_spec(&s).is_ok());
        s.argv.clear();
        assert!(validate_exec_spec(&s).is_err(), "empty argv");
        let mut s = spec(&["/bin/echo"]);
        s.argv[0] = String::new();
        assert!(validate_exec_spec(&s).is_err(), "empty argv[0]");
        let mut s = spec(&["/bin/echo"]);
        s.argv.push("a\0b".into());
        assert!(validate_exec_spec(&s).is_err(), "NUL in argv");
        let mut s = spec(&["/bin/echo"]);
        s.cwd = "tmp".into();
        assert!(validate_exec_spec(&s).is_err(), "relative cwd");
        let mut s = spec(&["/bin/echo"]);
        s.timeout_ms = 0;
        assert!(validate_exec_spec(&s).is_err(), "zero timeout");
        let mut s = spec(&["/bin/echo"]);
        s.timeout_ms = MAX_TIMEOUT_MS + 1;
        assert!(validate_exec_spec(&s).is_err(), "timeout beyond max");
        let mut s = spec(&["/bin/echo"]);
        s.env.push(("BAD=KEY".into(), "v".into()));
        assert!(validate_exec_spec(&s).is_err(), "'=' in env key");
    }

    #[test]
    fn canonical_body_digest_is_env_order_insensitive() {
        let mut a = spec(&["/bin/echo", "x"]);
        a.env = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
        let mut b = a.clone();
        b.env.reverse();
        let da = request_digest(&canonical_body(&a)).unwrap();
        let db = request_digest(&canonical_body(&b)).unwrap();
        assert_eq!(da, db, "env order must not change the identity");
        b.cwd = "/tmp".into();
        let dc = request_digest(&canonical_body(&b)).unwrap();
        assert_ne!(da, dc, "cwd is identity-bearing");
    }

    // ------------------------------------------------------ contract bridge

    #[test]
    fn contract_shell_materializes_to_dash_c_never_login_shell() {
        let cs = ls_core::contracts::ExecSpec::parse(&serde_json::json!({
            "command": {"kind": "shell", "executable": "/bin/dash", "script": "echo $HOME"},
            "cwd": "/tmp", "timeout_ms": 5000
        }))
        .unwrap();
        let s = spec_from_contract(&cs, None).unwrap();
        assert_eq!(s.argv, vec!["/bin/dash", "-c", "echo $HOME"]);
        assert_eq!(s.cwd, "/tmp");
        assert_eq!(s.timeout_ms, 5000);
        // argv form passes through untouched (spaces preserved)
        let cs = ls_core::contracts::ExecSpec::parse(&serde_json::json!({
            "command": {"kind": "argv", "argv": ["printf", "%s", "a b;$HOME"]}
        }))
        .unwrap();
        let s = spec_from_contract(&cs, None).unwrap();
        assert_eq!(s.argv, vec!["printf", "%s", "a b;$HOME"]);
        assert_eq!(s.cwd, "/");
        assert_eq!(s.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn resolve_user_rootfs_semantics() {
        let root = tmp_rootfs("users");
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(
            root.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\ndaemon:x:1:1:daemon:/usr/sbin:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(root.join("etc/group"), "root:x:0:\ndaemon:x:1:\nadm:x:4:\n").unwrap();
        assert_eq!(resolve_user(None, &root).unwrap(), None);
        assert_eq!(resolve_user(Some("0:0"), &root).unwrap(), Some((0, 0)));
        assert_eq!(resolve_user(Some("1000"), &root).unwrap(), Some((1000, 1000)));
        assert_eq!(resolve_user(Some("root"), &root).unwrap(), Some((0, 0)));
        assert_eq!(resolve_user(Some("daemon"), &root).unwrap(), Some((1, 1)));
        assert_eq!(resolve_user(Some("daemon:4"), &root).unwrap(), Some((1, 4)));
        assert_eq!(resolve_user(Some("daemon:adm"), &root).unwrap(), Some((1, 4)));
        assert!(resolve_user(Some("nobodyhere"), &root).is_err());
        assert!(resolve_user(Some("daemon:nosuchgroup"), &root).is_err());
        assert!(resolve_user(Some(":"), &root).is_err());
        // root with NO passwd database at all is the one tolerated literal
        let bare = tmp_rootfs("bare-root");
        assert_eq!(resolve_user(Some("root"), &bare).unwrap(), Some((0, 0)));
        assert!(resolve_user(Some("daemon"), &bare).is_err());
    }

    // -------------------------------------------------------------- policy

    struct FakePort {
        calls: AtomicUsize,
        script: std::sync::Mutex<VecDeque<ExitTruth>>,
        gate: Option<std::sync::Mutex<mpsc::Receiver<()>>>,
        env_seen: std::sync::Mutex<Vec<Vec<(String, String)>>>,
    }

    impl FakePort {
        fn new(script: Vec<ExitTruth>) -> Self {
            FakePort {
                calls: AtomicUsize::new(0),
                script: std::sync::Mutex::new(script.into()),
                gate: None,
                env_seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ExecPort for FakePort {
        fn exec(
            &self,
            _exec_id: &str,
            _instance: &ActiveInstance,
            spec: &ExecSpec,
        ) -> Result<ExecOutcome, DomainError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.env_seen.lock().unwrap().push(spec.env.clone());
            if let Some(rx) = &self.gate {
                rx.lock().unwrap().recv().ok();
            }
            let truth = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ExitTruth::Exited(0));
            Ok(ExecOutcome {
                truth,
                output: ExecOutput::default(),
                wall: Duration::from_millis(1),
                cli_exit: Some(0),
                receipt_text: None,
            })
        }
    }

    #[test]
    fn non_root_user_is_policy_refused_and_mints_nothing() {
        let rootfs = tmp_rootfs("policy");
        let svc = ExecService::new(FakePort::new(vec![]));
        let mut s = spec(&["/bin/true"]);
        s.user = Some((1000, 1000));
        let err = svc
            .start_execution(&owner(), &inst(&rootfs), "K1", s.clone())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PolicyUnsupported, "{err}");
        assert!(err.to_string().contains("maps exactly one uid"));
        // a refused request mints NO operation: replay refuses identically.
        assert!(svc
            .start_execution(&owner(), &inst(&rootfs), "K1", s)
            .is_err());
        // and (0,0) explicitly requested IS honored as container default.
        let mut s0 = spec(&["/bin/true"]);
        s0.user = Some((0, 0));
        let ok = svc
            .start_execution(&owner(), &inst(&rootfs), "K2", s0)
            .unwrap();
        assert_eq!(ok.view.state, ExecutionState::Exited);
    }

    // --------------------------------------------------------- state folding

    #[test]
    fn fresh_runs_once_and_folds_observed_truth() {
        let rootfs = tmp_rootfs("fold");
        let svc = ExecService::new(FakePort::new(vec![ExitTruth::Exited(7)]));
        let out = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/echo"]))
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::Exited);
        assert_eq!(
            out.view.exit_record,
            Some(ExitRecord::Exited { exit_code: 7 })
        );
        assert_eq!(out.view.termination, TerminationReason::None);
        assert!(!out.view.replayed);
    }

    #[test]
    fn spawn_failed_folds_to_start_failed_with_source_kept() {
        let rootfs = tmp_rootfs("sf");
        let svc = ExecService::new(FakePort::new(vec![ExitTruth::SpawnFailed {
            detail: "exec: ENOENT".into(),
            source: TruthSource::WitnessReceipt,
        }]));
        let out = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/nope"]))
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::StartFailed);
        assert_eq!(out.view.exit_record, None);
        let truth = &out.outcome.as_ref().unwrap().truth;
        assert!(matches!(
            truth,
            ExitTruth::SpawnFailed {
                source: TruthSource::WitnessReceipt,
                ..
            }
        ));
    }

    #[test]
    fn signaled_and_timeout_fold_through_the_domain_laws() {
        let rootfs = tmp_rootfs("sig");
        let svc = ExecService::new(FakePort::new(vec![
            ExitTruth::Signaled(11),
            ExitTruth::TimedOut { after_ms: 50 },
        ]));
        let a = svc
            .start_execution(&owner(), &inst(&rootfs), "Ka", spec(&["/bin/x"]))
            .unwrap();
        assert_eq!(a.view.state, ExecutionState::Exited);
        assert_eq!(
            a.view.exit_record,
            Some(ExitRecord::Signaled { signal: 11 })
        );
        let b = svc
            .start_execution(&owner(), &inst(&rootfs), "Kb", spec(&["/bin/x"]))
            .unwrap();
        // timeout law: unknown + the INDEPENDENT termination dimension
        assert_eq!(b.view.state, ExecutionState::Unknown);
        assert_eq!(b.view.exit_record, Some(ExitRecord::Unknown));
        assert_eq!(b.view.termination, TerminationReason::Timeout);
    }

    #[test]
    fn witness_lost_is_unknown_without_termination_reason() {
        let rootfs = tmp_rootfs("lost");
        let svc = ExecService::new(FakePort::new(vec![ExitTruth::WitnessLost {
            cli_exit: Some(139),
        }]));
        let out = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/x"]))
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::Unknown);
        assert_eq!(out.view.termination, TerminationReason::None);
    }

    // ------------------------------------------------------------ idempotency

    #[test]
    fn replay_same_key_same_request_returns_same_operation() {
        let rootfs = tmp_rootfs("idem");
        let svc = ExecService::new(FakePort::new(vec![]));
        let s = spec(&["/bin/echo", "hi"]);
        let a = svc
            .start_execution(&owner(), &inst(&rootfs), "K", s.clone())
            .unwrap();
        let b = svc
            .start_execution(&owner(), &inst(&rootfs), "K", s)
            .unwrap();
        assert_eq!(a.view.execution_id, b.view.execution_id);
        assert!(b.view.replayed);
        assert!(!a.view.replayed);
        // the effect ran ONCE even though the first was synchronous
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 1);
        // env ORDER must not fork the identity either
        let mut s2 = spec(&["/bin/echo", "hi"]);
        s2.env = vec![("B".into(), "2".into()), ("A".into(), "1".into())];
        svc.start_execution(&owner(), &inst(&rootfs), "Ke", s2.clone())
            .unwrap();
        s2.env.reverse();
        let again = svc
            .start_execution(&owner(), &inst(&rootfs), "Ke", s2)
            .unwrap();
        assert!(again.view.replayed);
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn same_key_different_request_is_conflict() {
        let rootfs = tmp_rootfs("conf");
        let svc = ExecService::new(FakePort::new(vec![]));
        svc.start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/a"]))
            .unwrap();
        let err = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/b"]))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        // a DIFFERENT key for the same request is a fresh execution
        let other = svc
            .start_execution(&owner(), &inst(&rootfs), "K2", spec(&["/bin/a"]))
            .unwrap();
        assert!(!other.view.replayed);
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn concurrent_replay_sees_starting_never_a_second_effect() {
        let rootfs = tmp_rootfs("conc");
        let (tx, rx) = mpsc::channel();
        let mut port = FakePort::new(vec![]);
        port.gate = Some(std::sync::Mutex::new(rx));
        let svc = Arc::new(ExecService::new(port));
        let owner = owner();
        let inst = inst(&rootfs);
        let spec = spec(&["/bin/slow"]);
        let (svc2, owner2, inst2, spec2) = (svc.clone(), owner.clone(), inst.clone(), spec.clone());
        let t = thread::spawn(move || {
            svc2.start_execution(&owner2, &inst2, "K", spec2).unwrap()
        });
        // let thread A actually enter the port
        for _ in 0..200 {
            if svc.port.calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 1);
        // replay while A is mid-flight: same operation, state starting, no 2nd effect
        let b = svc.start_execution(&owner, &inst, "K", spec).unwrap();
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 1);
        assert!(b.view.replayed);
        assert_eq!(b.view.state, ExecutionState::Starting);
        assert!(b.outcome.is_none());
        tx.send(()).unwrap();
        let a = t.join().unwrap();
        assert_eq!(a.view.state, ExecutionState::Exited);
        assert_eq!(a.view.execution_id, b.view.execution_id);
        assert_eq!(svc.port.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn get_execution_hides_other_principals_executions() {
        let rootfs = tmp_rootfs("get");
        let svc = ExecService::new(FakePort::new(vec![]));
        let out = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/x"]))
            .unwrap();
        let got = svc
            .get_execution(&owner(), &out.view.execution_id)
            .unwrap();
        assert_eq!(got.view.state, ExecutionState::Exited);
        let bob = OwnerId::new("bob").unwrap();
        let err = svc.get_execution(&bob, &out.view.execution_id).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "no existence probing");
        let ghost = ExecutionId::new("exec-ghost").unwrap();
        assert!(svc.get_execution(&owner(), &ghost).is_err());
    }

    // ------------------------------------------------------------- precheck

    #[test]
    fn precheck_reads_real_fs_metadata() {
        let rootfs = tmp_rootfs("pre");
        std::fs::create_dir_all(rootfs.join("bin")).unwrap();
        std::fs::create_dir_all(rootfs.join("sbin/dir")).unwrap();
        std::fs::write(rootfs.join("bin/ok"), b"#!/bin/true\n").unwrap();
        std::fs::write(rootfs.join("bin/noexec"), b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(rootfs.join("bin/ok"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::set_permissions(
            rootfs.join("bin/noexec"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(RuncWitnessExecPort::precheck(&rootfs, "/bin/ok").is_none());
        assert!(RuncWitnessExecPort::precheck(&rootfs, "bin/ok").is_none(), "relative passes through");
        let miss = RuncWitnessExecPort::precheck(&rootfs, "/bin/missing").unwrap();
        assert!(miss.contains("ENOENT"), "{miss}");
        let perm = RuncWitnessExecPort::precheck(&rootfs, "/bin/noexec").unwrap();
        assert!(perm.contains("execute bit"), "{perm}");
        let dir = RuncWitnessExecPort::precheck(&rootfs, "/sbin/dir").unwrap();
        assert!(dir.contains("directory"), "{dir}");
    }

    // ----------------------------------------------------------- drift guard

    #[test]
    fn build_command_mirrors_runc_exec_command() {
        // If RuncRuntime::exec_command's discipline ever changes, THIS test
        // turns red — the witness port must mirror the locked rootless
        // invocation exactly on the shared segment.
        let port = RuncWitnessExecPort::new(
            "/opt/toolchains/lightsandbox/bin/runc",
            "/var/lib/lightsandbox/runc-state",
            Some((65534, 65534)),
            "/home/nobody-xdg",
        );
        let runtime = RuncRuntime::new(
            "/opt/toolchains/lightsandbox/bin/runc",
            "/var/lib/lightsandbox/runc-state",
            Some((65534, 65534)),
            "/home/nobody-xdg",
        );
        let rootfs = tmp_rootfs("drift");
        let instance = inst(&rootfs);
        let mut s = spec(&["/bin/echo", "hello world"]);
        s.cwd = "/work".into();
        s.env = vec![("A".into(), "1".into()), ("B".into(), "x=y".into())];
        let exec_id = "exec-drift-1";

        let witness = RuncWitnessExecPort::witness_argv(&instance, &s, exec_id);
        let env_kvs: Vec<String> = s.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let reference =
            runtime.exec_command(&instance.container_id, &witness, &env_kvs, Some(&s.cwd));
        let ours = port.build_command(&instance, &s, exec_id);

        let ref_args: Vec<&OsStr> = reference.get_args().collect();
        let our_args: Vec<&OsStr> = ours.get_args().collect();
        assert_eq!(our_args, ref_args, "argv drift vs RuncRuntime::exec_command");

        let ref_env: Vec<(&OsStr, Option<&OsStr>)> = reference.get_envs().collect();
        let our_env: Vec<(&OsStr, Option<&OsStr>)> = ours.get_envs().collect();
        assert_eq!(our_env, ref_env, "env discipline drift");
    }

    // ------------------------------------------------- background (T11)

    use super::background::BackgroundSpawn;

    /// Scripted background port: spawn produces a REAL decoy sleeper (so
    /// BackgroundSpawn is construction-honest and always reaped), finish
    /// consumes scripted outcomes, optionally gated so the test can
    /// interleave cancel_execution with the in-flight wait.
    struct FakeBgPort {
        spawn_calls: AtomicUsize,
        fail_next_spawn: std::sync::atomic::AtomicBool,
        finishes: std::sync::Mutex<
            VecDeque<(
                Result<(ExecOutcome, BackgroundClock), DomainError>,
                Option<mpsc::Receiver<()>>,
            )>,
        >,
    }

    impl FakeBgPort {
        fn new() -> Self {
            FakeBgPort {
                spawn_calls: AtomicUsize::new(0),
                fail_next_spawn: std::sync::atomic::AtomicBool::new(false),
                finishes: std::sync::Mutex::new(VecDeque::new()),
            }
        }

        fn push(
            &self,
            truth: ExitTruth,
            clock: BackgroundClock,
            gate: Option<mpsc::Receiver<()>>,
        ) {
            self.finishes.lock().unwrap().push_back((
                Ok((
                    ExecOutcome {
                        truth,
                        output: ExecOutput::default(),
                        wall: Duration::from_millis(1),
                        cli_exit: Some(0),
                        receipt_text: None,
                    },
                    clock,
                )),
                gate,
            ));
        }
    }

    impl ExecPort for FakeBgPort {
        fn exec(
            &self,
            _exec_id: &str,
            _instance: &ActiveInstance,
            _spec: &ExecSpec,
        ) -> Result<ExecOutcome, DomainError> {
            panic!("background tests never enter the foreground door");
        }
    }

    impl BackgroundExecPort for FakeBgPort {
        fn spawn_background(
            &self,
            exec_id: &str,
            instance: &ActiveInstance,
            _spec: &ExecSpec,
        ) -> Result<BackgroundStart, DomainError> {
            self.spawn_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_next_spawn.swap(false, Ordering::SeqCst) {
                return Err(DomainError::new(
                    ErrorCode::InternalFailure,
                    "scripted spawn failure",
                ));
            }
            let mut child = Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("decoy sleeper spawns");
            let pid = child.id() as i32;
            let stdout_t = RuncWitnessExecPort::capture_pipe(child.stdout.take().unwrap());
            let stderr_t = RuncWitnessExecPort::capture_pipe(child.stderr.take().unwrap());
            Ok(BackgroundStart::Spawned(BackgroundSpawn {
                child,
                pid,
                stdout_t,
                stderr_t,
                started: Instant::now(),
                ctrl_host: super::background::host_ctrl_path(&instance.rootfs, exec_id),
            }))
        }

        fn finish_background(
            &self,
            mut spawn: BackgroundSpawn,
            _exec_id: &str,
            _instance: &ActiveInstance,
            _budget: Duration,
            _cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<(ExecOutcome, BackgroundClock), DomainError> {
            let (effect, gate) = self
                .finishes
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted finish available");
            if let Some(rx) = gate {
                rx.recv().ok();
            }
            // The decoy is never the subject; reap it so no orphan survives.
            let _ = spawn.child.kill();
            let _ = spawn.child.wait();
            let _ = spawn.stdout_t.join();
            let _ = spawn.stderr_t.join();
            effect
        }
    }

    fn bg_svc(port: FakeBgPort) -> Arc<ExecService<FakeBgPort>> {
        Arc::new(ExecService::new(port))
    }

    #[test]
    fn bg_spawn_failed_after_promote_settles_unknown_and_keeps_truth() {
        // The Running placeholder (chain spawned) can race a witness
        // SPAWN_FAILED: the frozen table forbids running→start-failed, so
        // the fold must settle Unknown with the observed truth preserved in
        // the outcome (the on-disk receipt is the reconcile path back).
        let rootfs = tmp_rootfs("bg-sf-late");
        let port = FakeBgPort::new();
        port.push(
            ExitTruth::SpawnFailed {
                detail: "cannot spawn entrypoint: ENOENT".to_string(),
                source: TruthSource::WitnessReceipt,
            },
            BackgroundClock::default(),
            None,
        );
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/nope"]),
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::Running);
        let eid = out.view.execution_id.clone();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(done.view.state, ExecutionState::Unknown);
        let truth = &done.outcome.as_ref().expect("outcome kept").truth;
        assert!(matches!(
            truth,
            ExitTruth::SpawnFailed {
                source: TruthSource::WitnessReceipt,
                ..
            }
        ));
        // replay of the same key returns the settled view, no respawn
        let again = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/nope"]),
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(again.view.replayed);
        assert_eq!(again.view.state, ExecutionState::Unknown);
        let _ = std::fs::remove_dir_all(&rootfs);
    }

    #[test]
    fn bg_cancel_midway_marks_cancelled_with_observed_signal() {
        let rootfs = tmp_rootfs("bg-cancel");
        // the ctrl dir exists in production (port precreates); fake writes
        // land best-effort — create it so fire_cancel observably succeeds.
        std::fs::create_dir_all(rootfs.join(".lsbx/exec")).unwrap();
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(ExitTruth::Signaled(9), BackgroundClock::default(), Some(rx));
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/long"]),
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::Running);
        assert!(out.outcome.is_none());
        let eid = out.view.execution_id.clone();
        // cancel while in flight: arms the flag, writes the kill switch.
        let c = svc.cancel_execution(&owner(), &eid).unwrap();
        assert_eq!(c.view.state, ExecutionState::Running);
        assert!(std::fs::read_to_string(super::background::host_ctrl_path(&rootfs, eid.as_str()))
            .unwrap()
            .starts_with("cancel"));
        // idempotent while in flight
        let c2 = svc.cancel_execution(&owner(), &eid).unwrap();
        assert!(c2.view.replayed);
        tx.send(()).unwrap();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(done.view.state, ExecutionState::Exited);
        assert_eq!(
            done.view.exit_record,
            Some(ExitRecord::Signaled { signal: 9 })
        );
        assert_eq!(done.view.termination, TerminationReason::Cancelled);
        // repeated cancel after cancelled-terminal: idempotent, same view
        let again = svc.cancel_execution(&owner(), &eid).unwrap();
        assert_eq!(again.view.termination, TerminationReason::Cancelled);
        let got = svc.get_execution(&owner(), &eid).unwrap();
        assert_eq!(got.view.termination, TerminationReason::Cancelled);
    }

    #[test]
    fn bg_cancel_after_natural_terminal_conflicts_and_record_stands() {
        let rootfs = tmp_rootfs("bg-late");
        let port = FakeBgPort::new();
        port.push(ExitTruth::Exited(0), BackgroundClock::default(), None);
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/quick"]),
                Duration::from_secs(60),
            )
            .unwrap();
        let eid = out.view.execution_id.clone();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(done.view.state, ExecutionState::Exited);
        assert_eq!(done.view.termination, TerminationReason::None);
        let err = svc.cancel_execution(&owner(), &eid).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict, "{err}");
        let got = svc.get_execution(&owner(), &eid).unwrap();
        assert_eq!(
            got.view.exit_record,
            Some(ExitRecord::Exited { exit_code: 0 })
        );
        assert_eq!(got.view.termination, TerminationReason::None);
    }

    #[test]
    fn bg_natural_exit_beats_racing_cancel_no_termination_mark() {
        let rootfs = tmp_rootfs("bg-race");
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(ExitTruth::Exited(0), BackgroundClock::default(), Some(rx));
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/race"]),
                Duration::from_secs(60),
            )
            .unwrap();
        let eid = out.view.execution_id.clone();
        svc.cancel_execution(&owner(), &eid).unwrap();
        tx.send(()).unwrap();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        // the kill demonstrably lost the race: observed natural exit stands,
        // NO Cancelled mark (spec card C04: terminal state must be stable).
        assert_eq!(
            done.view.exit_record,
            Some(ExitRecord::Exited { exit_code: 0 })
        );
        assert_eq!(done.view.termination, TerminationReason::None);
        let err = svc.cancel_execution(&owner(), &eid).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    #[test]
    fn bg_witness_lost_after_cancel_is_cancelled_unknown_never_retried() {
        let rootfs = tmp_rootfs("bg-lost");
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(
            ExitTruth::WitnessLost { cli_exit: None },
            BackgroundClock::default(),
            Some(rx),
        );
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/lost"]),
                Duration::from_secs(60),
            )
            .unwrap();
        let eid = out.view.execution_id.clone();
        svc.cancel_execution(&owner(), &eid).unwrap();
        tx.send(()).unwrap();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(done.view.state, ExecutionState::Unknown);
        assert_eq!(done.view.exit_record, Some(ExitRecord::Unknown));
        assert_eq!(done.view.termination, TerminationReason::Cancelled);
        // idempotent cancel replay of a cancelled-unknown execution
        let again = svc.cancel_execution(&owner(), &eid).unwrap();
        assert_eq!(again.view.termination, TerminationReason::Cancelled);
    }

    #[test]
    fn bg_deadline_attribution_beats_racing_cancel() {
        let rootfs = tmp_rootfs("bg-deadline");
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(
            ExitTruth::Signaled(9),
            BackgroundClock {
                deadline_fired: true,
            },
            Some(rx),
        );
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/slow"]),
                Duration::from_secs(60),
            )
            .unwrap();
        let eid = out.view.execution_id.clone();
        svc.cancel_execution(&owner(), &eid).unwrap();
        tx.send(()).unwrap();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            done.view.exit_record,
            Some(ExitRecord::Signaled { signal: 9 })
        );
        // outer policy horizon beats a racing cancel (set-once law)
        assert_eq!(done.view.termination, TerminationReason::Timeout);
    }

    #[test]
    fn bg_wait_budget_expiry_returns_running_and_never_cancels() {
        let rootfs = tmp_rootfs("bg-wait");
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(ExitTruth::Exited(9), BackgroundClock::default(), Some(rx));
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/calm"]),
                Duration::from_secs(60),
            )
            .unwrap();
        let eid = out.view.execution_id.clone();
        let early = svc
            .wait_execution(&owner(), &eid, Duration::from_millis(30))
            .unwrap();
        assert_eq!(early.view.state, ExecutionState::Running);
        assert!(early.outcome.is_none());
        tx.send(()).unwrap();
        let done = svc
            .wait_execution(&owner(), &eid, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            done.view.exit_record,
            Some(ExitRecord::Exited { exit_code: 9 })
        );
        assert_eq!(done.view.termination, TerminationReason::None);
    }

    #[test]
    fn bg_replay_never_double_spawns_and_conflicts_on_digest_mismatch() {
        let rootfs = tmp_rootfs("bg-idem");
        let port = FakeBgPort::new();
        let (tx, rx) = mpsc::channel();
        port.push(ExitTruth::Exited(0), BackgroundClock::default(), Some(rx));
        let svc = bg_svc(port);
        let s = spec(&["/bin/slow"]);
        let a = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                s.clone(),
                Duration::from_secs(60),
            )
            .unwrap();
        let b = svc
            .start_background_execution(&owner(), &inst(&rootfs), "K", s, Duration::from_secs(60))
            .unwrap();
        assert_eq!(a.view.execution_id, b.view.execution_id);
        assert!(b.view.replayed);
        assert_eq!(svc.port.spawn_calls.load(Ordering::SeqCst), 1);
        let err = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/other"]),
                Duration::from_secs(60),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        // budget validation
        assert!(svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "B0",
                spec(&["/bin/x"]),
                Duration::ZERO,
            )
            .is_err());
        assert!(svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "B1",
                spec(&["/bin/x"]),
                Duration::from_millis(MAX_TIMEOUT_MS + 1),
            )
            .is_err());
        tx.send(()).unwrap();
        let _ = svc.wait_execution(&owner(), &a.view.execution_id, Duration::from_secs(5));
    }

    #[test]
    fn bg_spawn_failure_is_start_failed_and_terminal() {
        let rootfs = tmp_rootfs("bg-spawnfail");
        let port = FakeBgPort::new();
        port.fail_next_spawn.store(true, Ordering::SeqCst);
        let svc = bg_svc(port);
        let out = svc
            .start_background_execution(
                &owner(),
                &inst(&rootfs),
                "K",
                spec(&["/bin/x"]),
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(out.view.state, ExecutionState::StartFailed);
        assert!(out.view.backend_error.is_some());
        let got = svc
            .wait_execution(&owner(), &out.view.execution_id, Duration::from_secs(1))
            .unwrap();
        assert_eq!(got.view.state, ExecutionState::StartFailed);
    }

    #[test]
    fn fg_cancel_conflicts_and_unknown_ids_are_not_found() {
        let rootfs = tmp_rootfs("fg-cancel");
        let svc = ExecService::new(FakePort::new(vec![]));
        let out = svc
            .start_execution(&owner(), &inst(&rootfs), "K", spec(&["/bin/x"]))
            .unwrap();
        let err = svc
            .cancel_execution(&owner(), &out.view.execution_id)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict, "{err}");
        let ghost = ExecutionId::new("exec-ghost").unwrap();
        assert_eq!(
            svc.cancel_execution(&owner(), &ghost).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(
            svc.wait_execution(&owner(), &ghost, Duration::from_millis(1))
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}
