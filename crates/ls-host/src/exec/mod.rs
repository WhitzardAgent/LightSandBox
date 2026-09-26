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

    /// The witness argv as the guest sees it.
    fn witness_argv(instance: &ActiveInstance, spec: &ExecSpec, exec_id: &str) -> Vec<String> {
        let mut v = vec![
            GUEST_INIT_PATH.to_string(),
            "--sandbox-id".to_string(),
            instance.sandbox_id.as_str().to_string(),
            "--receipt".to_string(),
            Self::guest_receipt_path(exec_id),
            "--".to_string(),
        ];
        v.extend(spec.argv.iter().cloned());
        v
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
        let guest = Self::witness_argv(instance, spec, exec_id);
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
        if let Some((uid, gid)) = self.identity {
            unsafe {
                cmd.pre_exec(move || {
                    if sys::setgid(gid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if sys::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // Own process group: a frontend timeout must be able to
                    // SIGKILL the runc CLI AND any in-container descendants
                    // it spawned (`kill -pgid`), never just the CLI shell.
                    if sys::setpgid(0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
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

        let truth = match receipt.termination {
            InitReceiptTermination::Exited { exit_code } => ExitTruth::Exited(exit_code as i32),
            InitReceiptTermination::Signaled { signal } => ExitTruth::Signaled(signal as u32),
            InitReceiptTermination::SpawnFailed { detail } => ExitTruth::SpawnFailed {
                detail,
                source: TruthSource::WitnessReceipt,
            },
            InitReceiptTermination::WaitInvariantBroken { .. } => unreachable!(),
        };
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
    reg: Mutex<Registry>,
    seq: std::sync::atomic::AtomicU64,
}

impl<P: ExecPort> ExecService<P> {
    pub fn new(port: P) -> Self {
        ExecService {
            port,
            reg: Mutex::new(Registry::default()),
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

    /// The one front door (spec §幂等与超时).
    pub fn start_execution(
        &self,
        principal: &OwnerId,
        instance: &ActiveInstance,
        operation_key: &str,
        spec: ExecSpec,
    ) -> Result<StartOutcome, DomainError> {
        // 1) request-shape validation — before any identity exists.
        validate_exec_spec(&spec)?;

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
        let digest = request_digest(&canonical_body(&spec))?;
        let key = Self::scope_key(&scope)?;

        // 4) identity decision under the single lock. Fresh mints a
        //    placeholder BEFORE any effect — a concurrent replay observes
        //    `starting` and can never double-fire.
        let exec_id = {
            let mut reg = self.reg.lock().expect("exec registry poisoned");
            let existing = reg.by_scope.get(&key).map(|r| &r.record);
            match decide(existing, &scope, &digest)? {
                ReplayDecision::Replay { result_ref } => {
                    let r = reg.by_scope.get(&key).expect("replay row");
                    debug_assert_eq!(r.execution.id().as_str(), result_ref);
                    return Ok(StartOutcome {
                        view: Self::view_of(r, true),
                        outcome: r.outcome.clone(),
                    });
                }
                ReplayDecision::Fresh => {
                    let exec_id = self.mint_exec_id()?;
                    // binding-only record: the Fencing/Manager binding lives
                    // in the lifecycle manager; Execution requires one.
                    let rec =
                        SandboxRecord::active(instance.sandbox_id.clone(), principal.clone());
                    let binding = ls_core::domain::access::ExecutionBinding::new(
                        exec_id.clone(),
                        &rec,
                    );
                    let mut execution = Execution::new(exec_id.clone(), binding);
                    execution.transition(ExecutionState::Starting)?;
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
                        },
                    );
                    reg.by_exec.insert(exec_id.as_str().to_string(), key.clone());
                    exec_id
                }
            }
        };

        // 5) the effect — strictly outside the lock.
        let effect: Result<ExecOutcome, DomainError> = self.port.exec(
            exec_id.as_str(),
            instance,
            &spec,
        );

        // 6) fold observed truth into the domain state machine; park the
        //    outcome. Domain errors here are internal by construction (the
        //    placeholder is always `starting` at this point).
        let mut reg = self.reg.lock().expect("exec registry poisoned");
        let r = reg.by_scope.get_mut(&key).expect("fresh row");
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
        Ok(StartOutcome {
            view: Self::view_of(r, false),
            outcome: r.outcome.clone(),
        })
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
}
