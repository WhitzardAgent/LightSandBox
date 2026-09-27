//! T11 — cancellation & process hierarchy for supervised BACKGROUND execs
//! (plans/tasks/T11.md; docs/v5/03_DOMAIN.md §命令与退出; docs/v5/08_FAILURES.md
//! §退出证据).
//!
//! The T10 foreground path is deliberately left byte-identical. Background
//! executions reuse the SAME witness + receipt protocol, with two protocol-v2
//! additions the guest init already implements (see crates/ls-guest):
//!
//! * `--nested-pidns`: the payload becomes ns-init of its own nested PID
//!   namespace, so killing IT is guaranteed by the kernel to take the whole
//!   descendant set with it — fork bombs, `setsid`, and double-fork cannot
//!   escape (C01). The host never walks a volatile PID tree.
//! * `--ctrl <guest path>`: the witness polls a host-writable ctrl file as
//!   its reliable kill switch. The host writes the kill verb; the witness
//!   SIGKILLs exactly the child it holds a waitpid handle on (no PID-reuse
//!   mistakes possible) and records what it really observed (SIGNALED).
//!
//! Attribution law (spec §10): the witness receipt only ever says what the
//! kernel wait path saw (Exited / Signaled / SpawnFailed). Whether that
//! terminates as `Cancelled`, `Timeout`, or nothing is decided HERE from
//! host-side clocks and flags:
//!
//! * deadline fired                        → Timeout
//! * cancel flag set (and kill observed)   → Cancelled
//! * cancel requested but natural exit won → the OBSERVED record stands,
//!   no Cancelled mark (the kill demonstrably did not cause the exit)
//! * deadline + cancel both raced          → Timeout wins (set-once, outer
//!   policy horizon beats a later cancel)
//! * evidence lost after cancel            → Cancelled attribution with an
//!   `unknown` exit record — never re-executed, per the unknown law.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ls_core::domain::error::{DomainError, ErrorCode};

use super::{
    ActiveInstance, ExecOutcome, ExecOutput, ExitTruth, RuncWitnessExecPort, TruthSource,
};

/// Kill verb the witness (protocol v2) recognizes in its ctrl file. Cancel
/// and timeout use the SAME verb on the wire — the action (SIGKILL the
/// supervised child) is identical; attribution is a host-side concern.
pub const WITNESS_KILL_VERB: &str = "cancel\n";

/// After the ctrl kill-switch fires, the witness is given this grace to die
/// and be reaped before the host falls back to signalling the runc CLI
/// process group directly (best-effort; a stuck witness degrades to the
/// T10 unknown law, never a fabricated result).
pub const TERMINATION_GRACE: Duration = Duration::from_secs(5);

/// Guest-side ctrl path the witness polls (protocol v2).
pub fn guest_ctrl_path(exec_id: &str) -> String {
    format!("/.lsbx/exec/{exec_id}.ctrl")
}

/// Host-side location of the same file inside the lease rootfs.
pub fn host_ctrl_path(rootfs: &Path, exec_id: &str) -> PathBuf {
    rootfs
        .join(".lsbx")
        .join("exec")
        .join(format!("{exec_id}.ctrl"))
}

/// A witness chain spawned for background supervision. The runc CLI child
/// and its bounded capture threads live here until the supervising waiter
/// thread (the only waitpid authority) adopts them via
/// [`BackgroundExecPort::finish_background`].
pub struct BackgroundSpawn {
    pub(crate) child: Child,
    pub(crate) pid: i32,
    pub(crate) stdout_t: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
    pub(crate) stderr_t: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
    pub(crate) started: Instant,
    pub(crate) ctrl_host: PathBuf,
}

impl std::fmt::Debug for BackgroundSpawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSpawn")
            .field("pid", &self.pid)
            .field("ctrl_host", &self.ctrl_host)
            .finish()
    }
}

/// Result of attempting to spawn a background witness. A host-side precheck
/// failure is still an ExecOutcome-truth (same honesty as the foreground
/// path), so it is not collapsed into a DomainError.
pub enum BackgroundStart {
    /// argv[0] failed the fs-metadata precheck: SpawnFailed truth, no child.
    PrecheckFailed(ExecOutcome),
    /// Witness chain is up; reaping authority transfers with the handle.
    Spawned(BackgroundSpawn),
}

/// Host-side clock facts the finish path contributes to attribution.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackgroundClock {
    /// the background budget horizon fired while still running.
    pub deadline_fired: bool,
}

/// Port extension for supervised background executions. The production
/// implementor is [`RuncWitnessExecPort`]; tests inject a scripted port.
pub trait BackgroundExecPort: super::ExecPort {
    fn spawn_background(
        &self,
        exec_id: &str,
        instance: &ActiveInstance,
        spec: &super::ExecSpec,
    ) -> Result<BackgroundStart, DomainError>;

    /// Adopt the spawned chain and wait until terminal observability: the
    /// child exits, the budget horizon fires (ctrl kill-switch → grace →
    /// group-kill fallback), or the wait path itself breaks. This thread is
    /// the sole reaper; cancellation is signalled via the ctrl file and the
    /// shared flag (read for attribution only).
    fn finish_background(
        &self,
        spawn: BackgroundSpawn,
        exec_id: &str,
        instance: &ActiveInstance,
        budget: Duration,
        cancel: &AtomicBool,
    ) -> Result<(ExecOutcome, BackgroundClock), DomainError>;
}

impl BackgroundExecPort for RuncWitnessExecPort {
    fn spawn_background(
        &self,
        exec_id: &str,
        instance: &ActiveInstance,
        spec: &super::ExecSpec,
    ) -> Result<BackgroundStart, DomainError> {
        // Host-side precheck: identical testimony semantics to foreground.
        if let Some(detail) = Self::precheck(&instance.rootfs, &spec.argv[0]) {
            return Ok(BackgroundStart::PrecheckFailed(ExecOutcome {
                truth: ExitTruth::SpawnFailed {
                    detail,
                    source: TruthSource::Precheck,
                },
                output: ExecOutput::default(),
                wall: Duration::from_millis(0),
                cli_exit: None,
                receipt_text: None,
            }));
        }

        // The cancel kill-switch must be writable the moment the witness
        // starts polling; pre-create the ctrl directory (the witness only
        // creates receipt parents at teardown time). Ownership: the witness
        // runs as the mapped rootless identity — make the dir writable by it.
        let exec_dir = instance.rootfs.join(".lsbx").join("exec");
        std::fs::create_dir_all(&exec_dir).map_err(|e| {
            DomainError::new(
                ErrorCode::InternalFailure,
                format!("bg ctrl dir {}: {e}", exec_dir.display()),
            )
        })?;
        chown_dir_to_identity(&exec_dir, instance)?;

        let extra = [
            "--nested-pidns".to_string(),
            "--ctrl".to_string(),
            guest_ctrl_path(exec_id),
        ];
        let mut cmd = self.build_command_with(instance, spec, exec_id, &extra);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| {
            DomainError::new(ErrorCode::InternalFailure, format!("runc bg spawn: {e}"))
        })?;
        let pid = child.id() as i32;
        let stdout_t = Self::capture_pipe(child.stdout.take().expect("piped"));
        let stderr_t = Self::capture_pipe(child.stderr.take().expect("piped"));
        Ok(BackgroundStart::Spawned(BackgroundSpawn {
            child,
            pid,
            stdout_t,
            stderr_t,
            started,
            ctrl_host: host_ctrl_path(&instance.rootfs, exec_id),
        }))
    }

    fn finish_background(
        &self,
        spawn: BackgroundSpawn,
        exec_id: &str,
        instance: &ActiveInstance,
        budget: Duration,
        _cancel: &AtomicBool,
    ) -> Result<(ExecOutcome, BackgroundClock), DomainError> {
        let BackgroundSpawn {
            mut child,
            pid,
            stdout_t,
            stderr_t,
            started,
            ctrl_host,
        } = spawn;

        // 1) wait with the budget horizon; on expiry fire the witness
        //    kill-switch (authoritative nested-pidns kill), then fall back
        //    to the T10 group kill if grace expires.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        let deadline = started + budget;
        let mut timed_out = false;
        let mut orphan_risk = false;
        let cli_status: Option<std::process::ExitStatus> = loop {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                // Authoritative path: the witness SIGKILLs its own child,
                // which — thanks to the nested PID namespace — takes the
                // whole execution tree down with it at kernel level.
                if let Err(e) = std::fs::write(&ctrl_host, WITNESS_KILL_VERB) {
                    eprintln!(
                        "ls-host: bg deadline kill-switch write {} failed: {e}",
                        ctrl_host.display()
                    );
                }
                match rx.recv_timeout(TERMINATION_GRACE) {
                    Ok(Ok(s)) => break Some(s),
                    Ok(Err(e)) => {
                        return Err(DomainError::new(
                            ErrorCode::InternalFailure,
                            format!("runc bg wait: {e}"),
                        ))
                    }
                    _ => {
                        // Witness ignored the switch: best-effort T10-style
                        // group kill, honestly flagged — descendants outside
                        // the group may outlive us inside the container.
                        orphan_risk = true;
                        unsafe {
                            let _ = super::sys::kill(-pid, 9);
                        }
                        match rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(Ok(s)) => break Some(s),
                            _ => break None,
                        }
                    }
                }
            }
            let step = (deadline - now).min(Duration::from_millis(50));
            match rx.recv_timeout(step) {
                Ok(Ok(s)) => break Some(s),
                Ok(Err(e)) => {
                    return Err(DomainError::new(
                        ErrorCode::InternalFailure,
                        format!("runc bg wait: {e}"),
                    ))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(DomainError::new(
                        ErrorCode::InternalFailure,
                        "bg waiter thread vanished",
                    ))
                }
            }
        };
        let wall = started.elapsed();

        // 2) join capture threads (EOF when the witness chain died).
        let join = |h: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>| match h.join() {
            Ok(Ok(v)) => Ok(v),
            _ => Err(DomainError::new(
                ErrorCode::InternalFailure,
                "bg capture thread failed",
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
        let clock = BackgroundClock {
            deadline_fired: timed_out,
        };
        if orphan_risk {
            eprintln!(
                "ls-host: bg exec {exec_id} ignored witness kill-switch; fell back to group kill — descendant survival possible inside sandbox"
            );
        }

        // 3) classification tail, shared verbatim with the foreground law:
        //    ctrl-mediated timeout is the ONE improvement over T10: when
        //    the witness receipt survives, its Signaled record is REAL
        //    waitpid evidence and may stand (host marks Timeout/Cancelled);
        //    only a lost receipt degrades to unknown.
        if timed_out {
            let outcome = match self.read_receipt(instance, exec_id)? {
                Some((receipt_text, receipt)) => {
                    let truth = self.truth_from_receipt(cli_exit, &receipt, &receipt_text)?;
                    ExecOutcome {
                        truth,
                        output,
                        wall,
                        cli_exit,
                        receipt_text: Some(receipt_text),
                    }
                }
                None => ExecOutcome {
                    truth: ExitTruth::TimedOut {
                        after_ms: budget.as_millis() as u64,
                    },
                    output,
                    wall,
                    cli_exit,
                    receipt_text: None,
                },
            };
            return Ok((outcome, clock));
        }

        let outcome = match self.read_receipt(instance, exec_id)? {
            Some((receipt_text, receipt)) => {
                let truth = self.truth_from_receipt(cli_exit, &receipt, &receipt_text)?;
                ExecOutcome {
                    truth,
                    output,
                    wall,
                    cli_exit,
                    receipt_text: Some(receipt_text),
                }
            }
            None => ExecOutcome {
                truth: ExitTruth::WitnessLost { cli_exit },
                output,
                wall,
                cli_exit,
                receipt_text: None,
            },
        };
        Ok((outcome, clock))
    }
}

/// `cancel` write from the service; idempotent via the swap guard at the
/// call site (never a second write). Errors are logged, not hidden: the
/// flag is what attribution reads, and the budget horizon still bounds the
/// execution if this switch is lost.
pub fn fire_cancel(ctrl_host: &Path) {
    if let Err(e) = std::fs::write(ctrl_host, WITNESS_KILL_VERB) {
        eprintln!(
            "ls-host: cancel kill-switch write {} failed: {e}",
            ctrl_host.display()
        );
    }
}

/// Cheap ownership fix for the ctrl directory: when this host process runs
/// privileged (authorized worker), chown to the mapped rootless identity so
/// the in-guest witness (uid 0 ↔ host 65534) can also create its receipt
/// here. Unprivileged daemons create it as themselves - already correct.
fn chown_dir_to_identity(dir: &Path, instance: &ActiveInstance) -> Result<(), DomainError> {
    if !instance.single_id_userns {
        return Ok(());
    }
    extern "C" {
        fn geteuid() -> u32;
        fn chown(path: *const std::os::raw::c_char, owner: u32, group: u32) -> i32;
    }
    // PORTABLE_ROOTLESS maps exactly one host id; on this runtime it is the
    // owner of the lease rootfs. Stat the rootfs itself instead of guessing.
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(&instance.rootfs).map_err(|e| {
        DomainError::new(
            ErrorCode::InternalFailure,
            format!("bg ctrl dir stat {}: {e}", instance.rootfs.display()),
        )
    })?;
    let owner = meta.uid();
    let group = meta.gid();
    if unsafe { geteuid() } == owner {
        return Ok(());
    }
    let c_path = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(|_| {
        DomainError::new(ErrorCode::InternalFailure, "bg ctrl dir has NUL byte".to_string())
    })?;
    let rc = unsafe { chown(c_path.as_ptr(), owner, group) };
    if rc != 0 {
        // Honest degrade: staying root-owned would make the witness's
        // receipt write fail later (WitnessLost), so surface it now.
        return Err(DomainError::new(
            ErrorCode::InternalFailure,
            format!(
                "bg ctrl dir chown {} -> {owner}:{group}: {}",
                dir.display(),
                io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

/// Shared flag accessor used by the service fold step (kept here so both
/// the production waiter and unit tests agree on ordering).
pub(crate) fn cancel_requested(flag: &AtomicBool) -> bool {
    flag.load(Ordering::SeqCst)
}
