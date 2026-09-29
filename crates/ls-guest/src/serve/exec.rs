//! Exec child lifecycle for the guest control agent.
//!
//! One exec = one direct child (`std::process::Command`, argv only — never a
//! shell) with stdout/stderr piped and stdin detached (`Stdio::null()`). Each
//! pipe gets one reader thread pushing ≤ 64 KiB chunks into a bounded
//! `SyncChannel`; when the channel is full the reader thread blocks, the
//! child blocks on its pipe write, and memory stays bounded at roughly
//! `capacity × 64 KiB` — OS-level backpressure with no byte loss.
//!
//! Cancellation v1 is `std::process::Child::kill` (SIGKILL) to the DIRECT
//! child only — that is the whole v1 mechanism for CANCEL, SIGNAL signo=9 and
//! timeout. TERM-then-KILL escalation and process-group cancellation need a
//! future libc signal layer; that is a registered follow-up.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::serve::protocol::{Body, ExecStart, Finished, Frame, MAX_CHUNK};

/// One item for the single writer path: either a frame to serialize, or the
/// shutdown marker (`Final`) whose frame is written before the writer exits.
pub enum OutItem {
    Frame(Frame),
    Final(Frame),
}

/// The ONE writer path: pulls items from the bounded channel and serializes
/// them onto the wire in order. Exits on `Final` or when every sender is
/// gone. Returns the first write error, if any.
pub fn run_writer<W: std::io::Write>(mut w: W, rx: std::sync::mpsc::Receiver<OutItem>) -> std::io::Result<()> {
    while let Ok(item) = rx.recv() {
        let frame = match item {
            OutItem::Frame(f) => f,
            OutItem::Final(f) => {
                crate::serve::protocol::write_frame(&mut w, &f)?;
                return Ok(());
            }
        };
        crate::serve::protocol::write_frame(&mut w, &frame)?;
    }
    Ok(())
}

/// Why the agent terminated (or is about to terminate) the child. Decides
/// the EXEC_FINISHED kind when the observed wait status is a kill we ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// No termination ordered; report the observed status as-is.
    Natural,
    Cancelled,
    /// SIGNAL frame requested signo 9 (SIGKILL) for this exec.
    KilledBySignal(u32),
    TimedOut,
}

/// An in-flight exec: the spawned child plus its pump threads and deadline.
pub struct Active {
    pub request_id: u32,
    child: Child,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    deadline: Option<Instant>,
    cause: Cause,
    pumps: Vec<JoinHandle<()>>,
}

impl Active {
    /// Spawn the child but do NOT start the output pumps yet: the caller must
    /// emit EXEC_ACCEPTED first ([`Active::start_pumps`] after that), so a
    /// fast child can never get a chunk ahead of its ACCEPTED on the wire.
    /// An empty cwd inherits the agent's own cwd (a wire-level empty cwd is
    /// never turned into a bogus chdir("")).
    pub fn spawn(spec: &ExecStart, request_id: u32) -> Result<Active, String> {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        // The wire decoder rejects argv_count = 0, but `spawn` is public:
        // never index-panic the guest agent on a caller-side mistake.
        let argv0_bytes = spec.argv.first().ok_or_else(|| "empty argv".to_string())?;
        let argv0 = std::ffi::OsStr::from_bytes(argv0_bytes);
        let mut cmd = Command::new(argv0);
        cmd.args(spec.argv[1..].iter().map(|a| std::ffi::OsStr::from_bytes(a)));
        if !spec.cwd.is_empty() {
            cmd.current_dir(std::ffi::OsString::from_vec(spec.cwd.clone()));
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", String::from_utf8_lossy(&spec.argv[0])))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let deadline = match spec.timeout_ms {
            0 => None,
            ms => Instant::now().checked_add(Duration::from_millis(ms as u64)),
        };
        Ok(Active {
            request_id,
            child,
            stdout,
            stderr,
            deadline,
            cause: Cause::Natural,
            pumps: Vec::new(),
        })
    }

    /// Start the two pipe-reader threads, forwarding into the shared writer
    /// channel. Called exactly once, right after EXEC_ACCEPTED was queued.
    pub fn start_pumps(&mut self, out: SyncSender<OutItem>) {
        if let Some(pipe) = self.stdout.take() {
            let tx = out.clone();
            let request_id = self.request_id;
            self.pumps
                .push(std::thread::spawn(move || pump(pipe, request_id, false, tx)));
        }
        if let Some(pipe) = self.stderr.take() {
            let tx = out.clone();
            let request_id = self.request_id;
            self.pumps
                .push(std::thread::spawn(move || pump(pipe, request_id, true, tx)));
        }
    }

    pub fn set_cause(&mut self, cause: Cause) {
        self.cause = cause;
    }

    /// Non-blocking reap; `Some(status)` means the child is reaped.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// SIGKILL the direct child (the v1 cancel/timeout/signal mechanism).
    /// An error (e.g. the child died on its own just before) only means the
    /// observation in `wait` is already final and honest.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Deadline reached with a timeout configured?
    pub fn timed_out(&self, now: Instant) -> bool {
        self.deadline.map(|d| now >= d).unwrap_or(false)
    }

    /// Terminate per `cause`, reap (no zombie leak on any terminal state),
    /// join the pump threads, and produce the honest FINISHED payload:
    /// the agent-ordered kind wins only when the observed wait status is a
    /// signal death; a natural exit observed alongside our kill is reported
    /// as the real exit code, never rewritten.
    pub fn finish(mut self) -> Finished {
        if self.cause != Cause::Natural {
            self.kill();
        }
        let status = self.child.wait();
        self.join_pumps();
        match status {
            Ok(status) => classify(&status, self.cause),
            Err(e) => Finished::SpawnFailed(format!("wait failed: {e}")),
        }
    }

    /// try_wait itself broke: an OS-level anomaly. Kill + reap best-effort
    /// (no zombie leak) and report the trouble as spawn-failed-class;
    /// never guess an exit code.
    pub fn fail_wait(mut self, why: String) -> Finished {
        self.kill();
        let _ = self.child.wait();
        self.join_pumps();
        Finished::SpawnFailed(why)
    }

    /// Kill + reap + join, discarding the classification. Used when the
    /// channel or the whole agent is going away and no FINISHED will be
    /// sent to anyone.
    pub fn terminate(mut self) {
        self.kill();
        let _ = self.child.wait();
        self.join_pumps();
    }

    fn join_pumps(&mut self) {
        for h in self.pumps.drain(..) {
            let _ = h.join();
        }
    }
}

/// Map an observed wait status plus the agent's cause to a FINISHED kind.
pub fn classify(status: &ExitStatus, cause: Cause) -> Finished {
    use std::os::unix::process::ExitStatusExt;
    let observed = match (status.code(), status.signal()) {
        (Some(code), _) => Finished::Exited(code),
        (None, Some(signo)) => Finished::Signaled(signo as u32),
        // Unreachable on unix (a wait status is always one of the two), but
        // reported honestly rather than guessed as 0.
        (None, None) => return Finished::SpawnFailed("wait status neither exit nor signal".into()),
    };
    match cause {
        Cause::Natural => observed,
        Cause::Cancelled => match observed {
            Finished::Signaled(_) => Finished::Cancelled,
            natural => natural,
        },
        Cause::TimedOut => match observed {
            Finished::Signaled(_) => Finished::Timeout,
            natural => natural,
        },
        Cause::KilledBySignal(_) => match observed {
            // Report the observed signal, not the requested one: SIGKILL
            // lands as 9; anything else observed is the truth of the wait.
            signaled @ Finished::Signaled(_) => signaled,
            natural => natural,
        },
    }
}

/// One pipe-reader thread: read up to 64 KiB, forward as a chunk, repeat to
/// EOF. Blocks on `tx.send` when the bounded channel is full — the child then
/// blocks on its pipe write (the backpressure contract).
fn pump<R: Read>(mut pipe: R, request_id: u32, is_err: bool, tx: SyncSender<OutItem>) {
    let mut buf = vec![0u8; MAX_CHUNK];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return, // EOF
            Ok(n) => {
                let body = if is_err {
                    Body::StderrChunk(buf[..n].to_vec())
                } else {
                    Body::StdoutChunk(buf[..n].to_vec())
                };
                if tx.send(OutItem::Frame(Frame::new(request_id, body))).is_err() {
                    return; // channel dropped (e.g. after SHUTDOWN_DONE)
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(argv: &[&[u8]], timeout_ms: u32) -> ExecStart {
        ExecStart {
            timeout_ms,
            cwd: b"/".to_vec(),
            argv: argv.iter().map(|a| a.to_vec()).collect(),
        }
    }

    #[test]
    fn spawn_failure_is_reported_not_raised() {
        let err = Active::spawn(&spec(&[b"/nonexistent/lsbx-void"], 0), 7).err().unwrap();
        assert!(err.contains("/nonexistent/lsbx-void"));
    }

    #[test]
    fn natural_exit_classifies_to_exit_code() {
        use std::process::Command;
        let _g = crate::TEST_CHILD_REAP_LOCK.lock().unwrap();
        let status = Command::new("/bin/sh").args(["-c", "exit 7"]).status().unwrap();
        assert_eq!(classify(&status, Cause::Natural), Finished::Exited(7));
    }

    #[test]
    fn killed_child_classifies_by_cause() {
        let _g = crate::TEST_CHILD_REAP_LOCK.lock().unwrap();
        for (cause, want) in [
            (Cause::Cancelled, Finished::Cancelled),
            (Cause::TimedOut, Finished::Timeout),
            (Cause::KilledBySignal(9), Finished::Signaled(9)),
        ] {
            let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
            let _ = child.kill();
            let status = child.wait().unwrap();
            assert_eq!(classify(&status, cause), want);
        }
        // A natural exit observed alongside our kill stays the truth.
        let status = Command::new("/bin/sh").args(["-c", "exit 0"]).status().unwrap();
        assert_eq!(classify(&status, Cause::Cancelled), Finished::Exited(0));
    }
}
