//! Tiny per-sandbox init (`lsguest-init`) — guest PID 1 for
//! launch_mode=image sandboxes (T08).
//!
//! Responsibilities, deliberately minimal:
//! * spawn the image entrypoint as a DIRECT child (argv only, never a shell);
//! * forward a fixed set of signals to that child (init itself never dies
//!   from them first);
//! * reap every descendant — PID 1 owns orphan adoption inside the pid
//!   namespace, so `runc exec`-spawned grandchildren can never accumulate
//!   as zombies;
//! * when the entrypoint child dies, record its REAL wait status — observed
//!   by `waitpid(2)` on this very path, never proxied — atomically to the
//!   receipt path (tmp + fsync + rename + dir fsync), then exit with the
//!   same wait semantics (same exit code, or 128+signal).
//!
//! The receipt is an INTERNAL host↔guest surface (not a frozen contract):
//! the host runtime reads it from the sandbox's writable rootfs layer.
//!
//! Zero external dependencies: the libc calls this needs (`waitpid`,
//! `sigaction`, `kill`, `_exit`) are declared directly against the system
//! libc so the crate stays buildable from an offline tree.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

// ---------------------------------------------------------------- exit codes

/// Usage/argument contract violation (same convention as ls-guest initargs).
pub const EXIT_USAGE: i32 = 2;
/// The entrypoint could not be spawned at all (exec not found / not
/// executable / resource failure). Distinct from the payload exiting 127:
/// the receipt kind carries the truth (SPAWN_FAILED vs EXITED).
pub const EXIT_SPAWN_FAILED: i32 = 127;

// ---------------------------------------------------------------- arguments

/// Verified arguments of the tiny init.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitArgs {
    /// Opaque server-generated sandbox id (never interpreted here).
    pub sandbox_id: String,
    /// Absolute guest path where the exit receipt is written.
    pub receipt: String,
    /// T11: absolute guest path of the cancel control file. When present,
    /// the wait loop polls it; content beginning with `cancel` orders this
    /// witness to SIGKILL its child (the nested-pidns init when
    /// `nested_pidns` is set — i.e. THIS execution's whole world, never a
    /// neighbour's). The file is host-writable within the same trust domain
    /// as the receipt path; the payload may self-cancel (own blast radius).
    pub ctrl: Option<String>,
    /// T11: run the payload as PID 1 of a freshly unshared NESTED PID
    /// namespace. Killing the namespace init wipes every descendant —
    /// doublefork/setsid progeny included — by kernel law, not by a
    /// volatile PID-tree scan. Cancellation scope = exactly this exec.
    pub nested_pidns: bool,
    /// Image entrypoint argv (entrypoint ++ cmd), never empty.
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitArgError {
    /// argv does not start with `--sandbox-id <v> --receipt <v> -- <argv...>`.
    BadShape(&'static str),
    InvalidSandboxId(&'static str),
    InvalidReceiptPath(&'static str),
    EmptyArgv,
}

impl fmt::Display for InitArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitArgError::BadShape(what) => write!(
                f,
                "expected `--sandbox-id <id> --receipt <abs-guest-path> -- <argv...>`: {what}"
            ),
            InitArgError::InvalidSandboxId(why) => write!(f, "invalid sandbox id: {why}"),
            InitArgError::InvalidReceiptPath(why) => write!(f, "invalid receipt path: {why}"),
            InitArgError::EmptyArgv => write!(f, "entrypoint argv after `--` must not be empty"),
        }
    }
}

impl std::error::Error for InitArgError {}

/// Parse exactly `--sandbox-id <id> --receipt <path> -- <argv...>`.
/// argv form only; no shell line ever reaches this binary.
pub fn parse_args(argv: &[String]) -> Result<InitArgs, InitArgError> {
    let mut it = argv.iter();
    match it.next().map(String::as_str) {
        Some("--sandbox-id") => {}
        _ => return Err(InitArgError::BadShape("missing leading --sandbox-id")),
    }
    let sandbox_id = match it.next() {
        Some(v) => v.clone(),
        None => return Err(InitArgError::BadShape("missing value after --sandbox-id")),
    };
    crate::initargs::validate_sandbox_id(&sandbox_id)
        .map_err(|e| InitArgError::InvalidSandboxId(match e {
            crate::initargs::InitArgError::InvalidSandboxId(why) => why,
            _ => "invalid",
        }))?;
    match it.next().map(String::as_str) {
        Some("--receipt") => {}
        _ => return Err(InitArgError::BadShape("missing --receipt")),
    }
    let receipt = match it.next() {
        Some(v) => v.clone(),
        None => return Err(InitArgError::BadShape("missing value after --receipt")),
    };
    validate_receipt_path(&receipt)?;
    // T11 (protocol v2, additive): zero or more optional flags before `--`.
    let mut ctrl = None;
    let mut nested_pidns = false;
    loop {
        match it.next().map(String::as_str) {
            Some("--") => break,
            Some("--ctrl") => {
                let path = match it.next() {
                    Some(v) => v.clone(),
                    None => return Err(InitArgError::BadShape("missing value after --ctrl")),
                };
                validate_receipt_path(&path).map_err(|e| match e {
                    InitArgError::InvalidReceiptPath(why) => InitArgError::InvalidReceiptPath(why),
                    other => other,
                })?;
                ctrl = Some(path);
            }
            Some("--nested-pidns") => nested_pidns = true,
            _ => {
                return Err(InitArgError::BadShape(
                    "missing `--` separator before entrypoint argv (unknown flag?)",
                ))
            }
        }
    }
    let argv: Vec<String> = it.cloned().collect();
    if argv.is_empty() {
        return Err(InitArgError::EmptyArgv);
    }
    Ok(InitArgs {
        sandbox_id,
        receipt,
        ctrl,
        nested_pidns,
        argv,
    })
}

/// Receipt path validation: absolute, component-wise clean (mirrors the
/// host-side GuestPath rule), never the root itself (it names a FILE).
pub fn validate_receipt_path(p: &str) -> Result<(), InitArgError> {
    if !p.starts_with('/') || p == "/" {
        return Err(InitArgError::InvalidReceiptPath("must be an absolute file path"));
    }
    if p.contains('\0') {
        return Err(InitArgError::InvalidReceiptPath("contains NUL"));
    }
    for comp in p.split('/').skip(1) {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(InitArgError::InvalidReceiptPath("illegal path component"));
        }
    }
    Ok(())
}

// -------------------------------------------------------- wait status decode

/// Wait-status decode of a reaped child, as observed by THIS init
/// (`waitpid(2)` on the direct path). Mutually exclusive; mirrors the
/// domain vocabulary "exit code | signaled | unknown" — unknown never
/// appears here because the observation is direct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedExit {
    Exited(i32),
    Signaled(i32),
}

impl ObservedExit {
    /// The code init itself exits with, preserving the child's semantics:
    /// same exit code, or 128+signal (shell/wait convention).
    pub fn init_exit_code(self) -> i32 {
        match self {
            ObservedExit::Exited(code) => code & 0xff,
            ObservedExit::Signaled(sig) => 128 + (sig & 0x7f),
        }
    }
}

/// Decode a raw `waitpid` status word. Stopped/continued/traced states are
/// not exits and must never reach the receipt path — they decode to None.
pub fn decode_wait_status(raw: i32) -> Option<ObservedExit> {
    // WIFEXITED: (raw & 0x7f) == 0 ; WEXITSTATUS: (raw & 0xff00) >> 8
    if raw & 0x7f == 0 {
        return Some(ObservedExit::Exited((raw & 0xff00) >> 8));
    }
    // WIFSIGNALED: low 7 bits != 0 and != 0x7f ; WTERMSIG: raw & 0x7f
    let low = raw & 0x7f;
    if low != 0x7f {
        return Some(ObservedExit::Signaled(low));
    }
    None
}

// ------------------------------------------------------------------- receipt

/// Render the exit receipt JSON. Field order is deterministic; values come
/// only from the direct waitpid observation. `sandbox_id` is pre-validated
/// to `[A-Za-z0-9_-]` so plain interpolation is injection-safe.
pub fn render_receipt(
    sandbox_id: &str,
    child_pid: i32,
    exit: ObservedExit,
    supervision: &SupervisionReport,
) -> String {
    let termination = match exit {
        ObservedExit::Exited(code) => format!("{{\"kind\":\"EXITED\",\"exit_code\":{code}}}"),
        ObservedExit::Signaled(sig) => format!("{{\"kind\":\"SIGNALED\",\"signal\":{sig}}}"),
    };
    format!(
        "{{\"version\":1,\"sandbox_id\":\"{sandbox_id}\",\"child_pid\":{child_pid},\
\"termination\":{termination},\"source\":\"init_waitpid\",\
\"supervision\":{{\"mode\":\"{}\",\"reaped_escapes\":{},\"residual_risk\":{}}}}}\n",
        supervision.mode.as_str(),
        supervision.reaped_escapes,
        supervision.residual_risk
    )
}

/// The spawn-failure receipt: init observed that exec of the entrypoint
/// FAILED; the payload never ran. Its termination kind is deliberately
/// distinct so a payload exit-127 is never confused with a missing binary.
pub fn render_spawn_failed_receipt(sandbox_id: &str, detail: &str) -> String {
    let mut esc = String::with_capacity(detail.len());
    for c in detail.chars() {
        match c {
            '"' => esc.push_str("\\\""),
            '\\' => esc.push_str("\\\\"),
            c if (c as u32) < 0x20 => esc.push_str(&format!("\\u{:04x}", c as u32)),
            c => esc.push(c),
        }
    }
    format!(
        "{{\"version\":1,\"sandbox_id\":\"{sandbox_id}\",\"child_pid\":null,\
\"termination\":{{\"kind\":\"SPAWN_FAILED\",\"detail\":\"{esc}\"}},\"source\":\"init_spawn\"}}\n"
    )
}

/// Atomic write: `<path>.tmp-<pid>` + fsync + rename + parent dir fsync.
/// Any failure is reported, never swallowed.
pub fn write_receipt_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

// ------------------------------------------------------------- libc syscall
//
// Boundaries: these are the ONLY foreign calls init makes. They service the
// wait/reap/forward/exit contract above; nothing here exposes a shell, argv
// string splitting, or environment interpretation.

#[allow(non_camel_case_types)]
mod sys {
    pub type c_int = i32;
    pub type pid_t = i32;
    pub type sighandler_t = usize;

    /// x86_64 glibc sigaction layout: handler, mask (1024 bits), flags, restorer.
    #[repr(C)]
    pub struct SigAction {
        pub sa_handler: sighandler_t,
        pub sa_mask: [u64; 16],
        pub sa_flags: c_int,
        pub sa_restorer: usize,
    }

    extern "C" {
        pub fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t;
        pub fn kill(pid: pid_t, sig: c_int) -> c_int;
        pub fn sigaction(signum: c_int, act: *const SigAction, oldact: *mut SigAction) -> c_int;
        pub fn unshare(flags: c_int) -> c_int;
        pub fn prctl(option: c_int, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> c_int;
        pub fn _exit(status: c_int) -> !;
        pub fn __errno_location() -> *mut c_int;
    }

    pub const EINTR: c_int = 4;
    pub const ECHILD: c_int = 10;
    pub const ESRCH: c_int = 3;
    pub const WNOHANG: c_int = 1;
    pub const SIGKILL: c_int = 9;
    /// linux/sched.h: children of the caller enter a fresh PID namespace.
    pub const CLONE_NEWPID: c_int = 0x2000_0000;
    /// linux/prctl.h: orphaned descendants are adopted by THIS process.
    pub const PR_SET_CHILD_SUBREAPER: c_int = 36;
    pub const SIGHUP: c_int = 1;
    pub const SIGINT: c_int = 2;
    pub const SIGQUIT: c_int = 3;
    pub const SIGTERM: c_int = 15;
    pub const SIGUSR1: c_int = 10;
    pub const SIGUSR2: c_int = 12;

    pub fn errno() -> c_int {
        unsafe { *__errno_location() }
    }
}

/// Signals init forwards to the entrypoint child. SIGKILL/SIGSTOP are not
/// listed (uncatchable); SIGCHLD is implicit in the waitpid loop.
pub const FORWARDED: [i32; 6] = [
    sys::SIGHUP,
    sys::SIGINT,
    sys::SIGQUIT,
    sys::SIGTERM,
    sys::SIGUSR1,
    sys::SIGUSR2,
];

/// Async-signal-safe mailbox: the handler body is a single atomic store.
static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward_handler(sig: sys::c_int) {
    PENDING_SIGNAL.store(sig, Ordering::SeqCst);
}

/// Install forwarding handlers for [`FORWARDED`]. Called exactly once
/// before spawning the child.
pub fn install_forward_handlers() {
    for &sig in &FORWARDED {
        let act = sys::SigAction {
            sa_handler: forward_handler as *const () as usize,
            sa_mask: [0u64; 16],
            sa_flags: 0, // no SA_RESTART: waitpid must return EINTR promptly
            sa_restorer: 0, // glibc sets the restorer on the rt_sigaction path
        };
        let rc = unsafe { sys::sigaction(sig, &act, std::ptr::null_mut()) };
        if rc != 0 {
            eprintln!(
                "lsguest-init: sigaction({sig}) failed: errno {}",
                sys::errno()
            );
            let code = EXIT_USAGE;
            unsafe { sys::_exit(code) }
        }
    }
}

fn take_pending_signal() -> Option<i32> {
    match PENDING_SIGNAL.swap(0, Ordering::SeqCst) {
        0 => None,
        s => Some(s),
    }
}

/// What the run loop concluded; distinguished so the receipt can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The entrypoint child was reaped; this is its observed exit.
    Child(ObservedExit),
    /// waitpid reported ECHILD before the child was seen — an invariant
    /// violation (the child is ours), surfaced honestly, never faked.
    WaitInvariantBroken(String),
}

impl RunOutcome {
    /// The exit code init terminates with for this outcome.
    pub fn init_exit_code(&self) -> i32 {
        match self {
            RunOutcome::Child(exit) => exit.init_exit_code(),
            RunOutcome::WaitInvariantBroken(_) => 70, // EX_SOFTWARE: runtime error
        }
    }
}

/// How this execution's progeny surface is supervised (v2.1). Receipt
/// evidence only — attribution of kills stays host-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervision {
    /// Nested PID namespace: the kernel wipes the whole world at kill.
    NestedPidns,
    /// PR_SET_CHILD_SUBREAPER adoption: escapes are reaped in the drain
    /// window; fork-bomb survivors are reported as residual risk.
    Subreaper,
    /// No adoption authority on this host: post-kill escape cleanup is the
    /// host's best-effort fallback; residual risk is reported on any kill.
    Direct,
}

impl Supervision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Supervision::NestedPidns => "nested_pidns",
            Supervision::Subreaper => "subreaper",
            Supervision::Direct => "direct",
        }
    }
}

/// Pure capability decision table (syscall-free, unit-testable): a
/// requested nested namespace that the host denies degrades to subreaper,
/// never to a fabricated spawn failure.
pub fn supervision_mode(requested: bool, unshare_rc: i32, prctl_rc: i32) -> Supervision {
    if requested && unshare_rc == 0 {
        Supervision::NestedPidns
    } else if requested && prctl_rc == 0 {
        Supervision::Subreaper
    } else {
        Supervision::Direct
    }
}

/// What the run actually supervised, reported alongside the exit truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionReport {
    pub mode: Supervision,
    /// Escape descendants reaped in the drain window (subreaper mode).
    pub reaped_escapes: u32,
    /// true = escape descendants may still be alive (drain window closed,
    /// waitpid broke, or a kill was ordered without adoption authority).
    pub residual_risk: bool,
}

/// Syscall shell around [`supervision_mode`]: attempt the nested namespace;
/// on denial try subreaper adoption; report the chosen mode on stderr.
fn establish_supervision(requested: bool) -> Supervision {
    if !requested {
        return Supervision::Direct;
    }
    let unshare_rc = unsafe { sys::unshare(sys::CLONE_NEWPID) };
    if unshare_rc == 0 {
        return Supervision::NestedPidns;
    }
    let err = sys::errno();
    let prctl_rc = unsafe { sys::prctl(sys::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    let mode = supervision_mode(true, unshare_rc, prctl_rc);
    match mode {
        Supervision::NestedPidns => unreachable!("unshare_rc != 0 on this branch"),
        Supervision::Subreaper => eprintln!(
            "lsguest-init: nested pidns unavailable (errno {err}); subreaper supervision active"
        ),
        Supervision::Direct => eprintln!(
            "lsguest-init: nested pidns unavailable (errno {err});              subreaper unavailable (errno {}); escape cleanup is best-effort",
            sys::errno()
        ),
    }
    mode
}

/// Spawn the entrypoint as a direct child (argv only — no shell), then run
/// the reap/forward loop until the child exits. stdio inside the guest is
/// inherited exactly as the runtime arranged it for PID 1. Returns the
/// child pid and the outcome; the caller writes the receipt and exits.
///
/// T11 v2 semantics (additive, only when the flags are present):
/// * `nested_pidns`: attempt `unshare(CLONE_NEWPID)` BEFORE the spawn. v2.1
///   degradation chain: a denied unshare (e.g. runc exec MINIMAL_CAPS drops
///   CAP_SYS_ADMIN, observed errno 1 on the current host) is NOT a spawn
///   failure — the witness degrades to subreaper adoption (or direct
///   best-effort), still runs the payload, and reports the real mode in the
///   receipt. Only pre-payload evidence that the ENTRYPOINT ITSELF could not
///   be exec'd remains a spawn failure.
/// * `ctrl`: the wait loop polls the cancel control file every 20 ms with
///   WNOHANG; a `cancel` prefix SIGKILLs OUR child (the namespace init when
///   nested — kernel then wipes that whole world). Receipt stays the
///   honest wait observation (SIGNALED 9); host-side attribution of who
///   ordered the kill is the host's own affair, never guessed here.
/// * supervision (subreaper mode): a 1 s drain window after the child is
///   reaped adopts+reaps escape descendants; a window closing with live
///   descendants reports residual_risk (fork-bomb truth, never hidden).
pub fn run_child(args: &InitArgs) -> Result<(i32, RunOutcome, SupervisionReport), SpawnFailure> {
    install_forward_handlers();
    let mode = establish_supervision(args.nested_pidns);
    let child = Command::new(&args.argv[0])
        .args(&args.argv[1..])
        .spawn()
        .map_err(|e| SpawnFailure {
            argv0: args.argv[0].clone(),
            detail: format!("{e}"),
        })?;
    let child_pid = child.id() as i32;
    // The waitpid loop below is authoritative for this pid. std::process::Child
    // must never also reap it; dropping without wait is the documented way.
    drop(child);

    let (pid, outcome, kill_sent) = match &args.ctrl {
        None => {
            let (p, o) = reap_blocking(child_pid);
            (p, o, false)
        }
        Some(ctrl) => reap_with_ctrl(child_pid, ctrl),
    };
    let (reaped_escapes, residual_risk) = match mode {
        Supervision::Subreaper => {
            let sidecar = std::path::PathBuf::from(format!("{}.drain.log", args.receipt));
            drain_escapes_logged(Some(&sidecar))
        }
        Supervision::NestedPidns => (0, false), // the kernel wipes the world
        Supervision::Direct => (0, kill_sent), // kill without adoption: unknown tail
    };
    Ok((
        pid,
        outcome,
        SupervisionReport {
            mode,
            reaped_escapes,
            residual_risk,
        },
    ))
}

/// Decode the reaped status of OUR child into a RunOutcome::Child.
fn child_outcome(child_pid: i32, status: sys::c_int) -> (i32, RunOutcome) {
    (
        child_pid,
        RunOutcome::Child(decode_wait_status(status).unwrap_or(ObservedExit::Exited(255))),
    )
}

/// T10 semantics: blocking reap/forward loop (no control file).
fn reap_blocking(child_pid: i32) -> (i32, RunOutcome) {
    loop {
        let mut status: sys::c_int = 0;
        let reaped = unsafe { sys::waitpid(-1, &mut status, 0) };
        if reaped == child_pid {
            return child_outcome(child_pid, status);
        }
        if reaped > 0 {
            // Orphaned grandchild reaped (PID 1 adopts). Keep looping.
            continue;
        }
        let err = sys::errno();
        if err == sys::EINTR {
            if let Some(sig) = take_pending_signal() {
                // The child may have exited between our last waitpid and
                // this kill; that is not an error worth faking — the next
                // waitpid iteration settles the truth.
                let _ = unsafe { sys::kill(child_pid, sig) };
            }
            continue;
        }
        if err == sys::ECHILD {
            return (
                child_pid,
                RunOutcome::WaitInvariantBroken(
                    "waitpid(-1) returned ECHILD while the entrypoint child was unseen".to_string(),
                ),
            );
        }
        return (
            child_pid,
            RunOutcome::WaitInvariantBroken(format!("waitpid(-1) failed with errno {err}")),
        );
    }
}

/// T11 semantics: WNOHANG + 20 ms ctrl poll. A `cancel`-prefixed ctrl file
/// SIGKILLs our child (idempotent: ESRCH is ignored, reap settles truth).
/// Third tuple member: a cancel kill was ordered (drain/residual input).
fn reap_with_ctrl(child_pid: i32, ctrl: &str) -> (i32, RunOutcome, bool) {
    let mut kill_sent = false;
    loop {
        let mut status: sys::c_int = 0;
        let reaped = unsafe { sys::waitpid(-1, &mut status, sys::WNOHANG) };
        if reaped == child_pid {
            let (p, o) = child_outcome(child_pid, status);
            return (p, o, kill_sent);
        }
        if reaped > 0 {
            continue; // adopted orphan reaped; keep supervising our child
        }
        if reaped < 0 {
            let err = sys::errno();
            if err == sys::EINTR {
                if let Some(sig) = take_pending_signal() {
                    let _ = unsafe { sys::kill(child_pid, sig) };
                }
            } else if err == sys::ECHILD {
                return (
                    child_pid,
                    RunOutcome::WaitInvariantBroken(
                        "waitpid(-1) returned ECHILD while the entrypoint child was unseen"
                            .to_string(),
                    ),
                    kill_sent,
                );
            } else {
                return (
                    child_pid,
                    RunOutcome::WaitInvariantBroken(format!("waitpid(-1) failed with errno {err}")),
                    kill_sent,
                );
            }
        }
        if !kill_sent {
            match std::fs::read_to_string(ctrl) {
                Ok(body) if body.starts_with("cancel") => {
                    kill_sent = true;
                    let rc = unsafe { sys::kill(child_pid, sys::SIGKILL) };
                    if rc != 0 && sys::errno() != sys::ESRCH {
                        // Kill of OUR OWN child failed for a non-race reason:
                        // surface it as a broken invariant rather than claim a
                        // cancel that did not happen.
                        return (
                            child_pid,
                            RunOutcome::WaitInvariantBroken(format!(
                                "cancel kill({child_pid}, SIGKILL) failed: errno {}",
                                sys::errno()
                            )),
                            kill_sent,
                        );
                    }
                }
                _ => {} // missing/empty/short-lived read error: keep polling
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Post-exit drain for the subreaper mode. Two duties, one honest report:
/// (a) REAP adopted escape descendants as they die; (b) SIGKILL the ones
/// still ALIVE — the execution's world is over, and under subreaper adoption
/// the witness is the only in-guest authority that can still reach
/// reparented escapees (a nested pidns would have let the kernel wipe them
/// atomically; we only have adoption, so we must aim). The kill list comes
/// from /proc/<pid>/stat's ppid field scanned for OUR pid; ESRCH on kill is
/// a win, not an error (the child raced us into the grave and waitpid
/// settles it). Unreadable /proc degrades to wait-only with residual risk
/// reported. The 1 s window bounds kill+reap cycles; survivors past it are
/// fork-bomb truth — reported, never hidden.
fn drain_escapes_logged(log: Option<&std::path::Path>) -> (u32, bool) {
    let note = |msg: String| {
        if let Some(p) = log {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{msg}");
            }
        }
    };
    const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(1000);
    const STEP: std::time::Duration = std::time::Duration::from_millis(20);
    let me = std::process::id() as i32;
    let start = std::time::Instant::now();
    let mut reaped: u32 = 0;
    note(format!(
        "drain start: me={me} proc_faithful={}",
        proc_is_faithful(me)
    ));
    loop {
        // 1) reap everything already dead; ECHILD means a clean world.
        loop {
            let mut status: sys::c_int = 0;
            let r = unsafe { sys::waitpid(-1, &mut status, sys::WNOHANG) };
            if r > 0 {
                reaped = reaped.saturating_add(1);
                continue;
            }
            if r < 0 {
                let err = sys::errno();
                if err == sys::ECHILD {
                    note(format!("drain: ECHILD clean world, reaped={reaped}"));
                    return (reaped, false);
                }
                if err == sys::EINTR {
                    continue;
                }
                note(format!("drain: waitpid error errno={err}, reaped={reaped}"));
                return (reaped, true); // waitpid broken: cannot know
            }
            break; // r == 0: live descendants exist below us
        }
        if start.elapsed() >= DRAIN_BUDGET {
            note(format!(
                "drain budget closed: reaped={reaped}, LIVE descendants remain"
            ));
            return (reaped, true);
        }
        // 2) aim at every live adopted descendant, then let them die.
        let landed = kill_adopted_descendants(me);
        note(format!(
            "drain sweep: killed={landed} reaped={reaped} elapsed_ms={}",
            start.elapsed().as_millis()
        ));
        std::thread::sleep(STEP);
    }
}

/// /proc fidelity self-check: /proc/self must describe THIS process with
/// THIS pid. A host-rbind /proc (Native v1, ADR 0001 A1) shows the HOST
/// process table: pid fields are foreign-namespace values and aiming by
/// `me` would either miss or — worse — aim at a host process that happens
/// to share the number. Untrusted /proc means wait-only + residual risk.
fn proc_is_faithful(me: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return false;
    };
    let Some(open) = stat.find('(') else {
        return false;
    };
    stat[..open]
        .trim()
        .parse::<i32>()
        .map(|pid| pid == me)
        .unwrap_or(false)
}

/// SIGKILL every /proc entry whose CURRENT ppid is `me` (our adopted
/// escapees — never the whole container, never a guessed process group).
/// The count is observational only; ESRCH is a corpse the next waitpid
/// reaps, any other error still lands in the residual-risk accounting via
/// the caller's window.
fn kill_adopted_descendants(me: i32) -> u32 {
    let mut landed = 0u32;
    if !proc_is_faithful(me) {
        return 0; // foreign /proc: cannot aim honestly — wait-only
    }
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return 0; // no /proc: no visibility — wait-only, residual risk
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue; // raced out of existence
        };
        // pid (comm) state ppid ... — comm may contain spaces/parens; the
        // state+ppid pair begins after the LAST ')'.
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut fields = stat[close + 1..].split_whitespace();
        let _state = fields.next();
        let Some(ppid) = fields.next().and_then(|f| f.parse::<i32>().ok()) else {
            continue;
        };
        if ppid != me {
            continue;
        }
        let rc = unsafe { sys::kill(pid, sys::SIGKILL) };
        if rc == 0 || sys::errno() == sys::ESRCH {
            landed = landed.saturating_add(1);
        }
    }
    landed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnFailure {
    pub argv0: String,
    pub detail: String,
}

impl fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot spawn entrypoint {:?}: {}", self.argv0, self.detail)
    }
}

impl std::error::Error for SpawnFailure {}

/// Full binary behaviour, split from `main` so it takes plain argv (unit
/// tests exercise it without PID 1). Returns the process exit code.
pub fn main_entry(argv: Vec<String>) -> i32 {
    let parsed = match parse_args(&argv[1..]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lsguest-init: usage error: {e}");
            return EXIT_USAGE;
        }
    };
    let receipt_path = PathBuf::from(&parsed.receipt);
    let report_spawn_fail = |detail: &str| {
        eprintln!("lsguest-init: {detail}");
        let body = render_spawn_failed_receipt(&parsed.sandbox_id, detail);
        if let Err(e) = write_receipt_atomic(&receipt_path, &body) {
            eprintln!(
                "lsguest-init: receipt write failed ({}): {e}",
                receipt_path.display()
            );
        }
    };
    match run_child(&parsed) {
        Ok((child_pid, RunOutcome::Child(exit), supervision)) => {
            let body = render_receipt(&parsed.sandbox_id, child_pid, exit, &supervision);
            if let Err(e) = write_receipt_atomic(&receipt_path, &body) {
                // The wait-observation is real but persistently unrecordable;
                // never fake a receipt. The exit code still carries truth.
                eprintln!(
                    "lsguest-init: receipt write failed ({}): {e}",
                    receipt_path.display()
                );
            }
            exit.init_exit_code()
        }
        Ok((_, RunOutcome::WaitInvariantBroken(detail), _)) => {
            report_spawn_fail(&format!("wait invariant broken: {detail}"));
            RunOutcome::WaitInvariantBroken(detail).init_exit_code()
        }
        Err(spawn_err) => {
            report_spawn_fail(&spawn_err.to_string());
            EXIT_SPAWN_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_exact_shape() {
        let p = parse_args(&argv(&[
            "--sandbox-id",
            "sbx_01234567",
            "--receipt",
            "/.lsbx/receipt.json",
            "--",
            "/bin/sleep",
            "600",
        ]))
        .unwrap();
        assert_eq!(p.sandbox_id, "sbx_01234567");
        assert_eq!(p.receipt, "/.lsbx/receipt.json");
        assert_eq!(p.argv, vec!["/bin/sleep".to_string(), "600".to_string()]);
    }

    #[test]
    fn rejects_malformed_shapes() {
        assert!(matches!(
            parse_args(&argv(&[])),
            Err(InitArgError::BadShape(_))
        ));
        assert!(matches!(
            parse_args(&argv(&["--sandbox-id", "sbx_01234567", "--receipt"])),
            Err(InitArgError::BadShape(_))
        ));
        assert!(matches!(
            parse_args(&argv(&[
                "--sandbox-id",
                "sbx_01234567",
                "--receipt",
                "/r.json"
            ])),
            Err(InitArgError::BadShape(_))
        ));
        assert!(matches!(
            parse_args(&argv(&[
                "--sandbox-id",
                "sbx_01234567",
                "--receipt",
                "/r.json",
                "--"
            ])),
            Err(InitArgError::EmptyArgv)
        ));
        assert!(matches!(
            parse_args(&argv(&[
                "--sandbox-id",
                "bad id!",
                "--receipt",
                "/r.json",
                "--",
                "/bin/true"
            ])),
            Err(InitArgError::InvalidSandboxId(_))
        ));
    }

    #[test]
    fn receipt_path_rules() {
        assert!(validate_receipt_path("/.lsbx/receipt.json").is_ok());
        assert!(validate_receipt_path("relative.json").is_err());
        assert!(validate_receipt_path("/").is_err());
        assert!(validate_receipt_path("/a/../b").is_err());
        assert!(validate_receipt_path("/a//b").is_err());
    }

    #[test]
    fn wait_status_decoding_is_exact() {
        // exit(7): status word = 7 << 8 = 0x700
        assert_eq!(decode_wait_status(0x700), Some(ObservedExit::Exited(7)));
        assert_eq!(decode_wait_status(0), Some(ObservedExit::Exited(0)));
        // killed by SIGKILL(9): low 7 bits = 9
        assert_eq!(decode_wait_status(9), Some(ObservedExit::Signaled(9)));
        // killed by SIGTERM(15)
        assert_eq!(decode_wait_status(15), Some(ObservedExit::Signaled(15)));
        // stopped (WIFSTOPPED, low byte 0x7f): not an exit
        assert_eq!(decode_wait_status(0x137f), None);
    }

    #[test]
    fn init_exit_code_preserves_child_semantics() {
        assert_eq!(ObservedExit::Exited(7).init_exit_code(), 7);
        assert_eq!(ObservedExit::Exited(0).init_exit_code(), 0);
        assert_eq!(ObservedExit::Signaled(15).init_exit_code(), 143);
        assert_eq!(ObservedExit::Signaled(9).init_exit_code(), 137);
    }

    fn sup_report(mode: Supervision) -> SupervisionReport {
        SupervisionReport {
            mode,
            reaped_escapes: 0,
            residual_risk: false,
        }
    }

    #[test]
    fn receipt_rendering_is_deterministic_and_typed() {
        let body = render_receipt("sbx_01234567", 4242, ObservedExit::Exited(7), &sup_report(Supervision::Direct));
        assert!(body.contains("\"sandbox_id\":\"sbx_01234567\""));
        assert!(body.contains("\"child_pid\":4242"));
        assert!(body.contains("\"termination\":{\"kind\":\"EXITED\",\"exit_code\":7}"));
        assert!(body.contains("\"source\":\"init_waitpid\""));
        assert!(body.contains(
            "\"supervision\":{\"mode\":\"direct\",\"reaped_escapes\":0,\"residual_risk\":false}"
        ));
        let sig = render_receipt("sbx_01234567", 4242, ObservedExit::Signaled(15), &sup_report(Supervision::Subreaper));
        assert!(sig.contains("\"termination\":{\"kind\":\"SIGNALED\",\"signal\":15}"));
        assert!(sig.contains("\"mode\":\"subreaper\""));
        let failed = render_spawn_failed_receipt("sbx_01234567", "spawn failed: \"no such file\"\nline2");
        assert!(failed.contains("\"kind\":\"SPAWN_FAILED\""));
        assert!(failed.contains("\\\""));
        assert!(failed.contains("\\u000a"));
    }

    #[test]
    fn receipt_write_is_atomic_via_tmp_rename() {
        let dir = std::env::temp_dir().join(format!(
            "lsguest-init-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("sub").join("receipt.json");
        let body = render_receipt("sbx_01234567", 1, ObservedExit::Exited(3), &sup_report(Supervision::Direct));
        write_receipt_atomic(&target, &body).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), body);
        // no tmp file lingers next to the receipt
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "tmp leftovers: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_failure_yields_spawn_failed_receipt_and_127() {
        // A non-existent entrypoint must NEVER be reported as a payload
        // exit: the receipt kind is SPAWN_FAILED with the OS error text.
        let args = InitArgs {
            sandbox_id: "sbx_01234567".to_string(),
            receipt: std::env::temp_dir()
                .join(format!("lsguest-init-spawnfail-{}.json", std::process::id()))
                .to_string_lossy()
                .to_string(),
            ctrl: None,
            nested_pidns: false,
            argv: vec!["/definitely/not-present-lsguest".to_string()],
        };
        let err = run_child(&args).unwrap_err();
        assert_eq!(err.argv0, "/definitely/not-present-lsguest");
        assert!(err.detail.contains("No such file") || err.detail.contains("ENOENT"));
        let body = render_spawn_failed_receipt(&args.sandbox_id, &err.to_string());
        assert!(body.contains("SPAWN_FAILED"));
        write_receipt_atomic(Path::new(&args.receipt), &body).unwrap();
        let _ = std::fs::remove_file(&args.receipt);
    }

    // waitpid(-1) is process-global, including across the serve module's
    // parallel tests. Hold the crate-wide guard around every child run.

    fn base_args(ctrl: Option<String>, nested: bool, argv: &[&str]) -> InitArgs {
        InitArgs {
            sandbox_id: "sbx_01234567".to_string(),
            receipt: "/tmp/lsguest-test-unused-receipt.json".to_string(),
            ctrl,
            nested_pidns: nested,
            argv: argv.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn parse_v1_shape_without_new_flags_is_unchanged() {
        let parsed = parse_args(&[
            "--sandbox-id".into(),
            "sbx_01234567".into(),
            "--receipt".into(),
            "/.lsbx/exec/e1.receipt.json".into(),
            "--".into(),
            "/bin/echo".into(),
            "hi".into(),
        ])
        .unwrap();
        assert_eq!(parsed.ctrl, None);
        assert!(!parsed.nested_pidns);
        assert_eq!(parsed.argv, vec!["/bin/echo", "hi"]);
    }

    #[test]
    fn parse_v2_flags_and_rejects_bad_shapes() {
        let parsed = parse_args(&[
            "--sandbox-id".into(),
            "sbx_01234567".into(),
            "--receipt".into(),
            "/.lsbx/exec/e1.receipt.json".into(),
            "--nested-pidns".into(),
            "--ctrl".into(),
            "/.lsbx/exec/e1.ctrl".into(),
            "--".into(),
            "/bin/sleep".into(),
            "9".into(),
        ])
        .unwrap();
        assert_eq!(parsed.ctrl.as_deref(), Some("/.lsbx/exec/e1.ctrl"));
        assert!(parsed.nested_pidns);
        assert_eq!(parsed.argv, vec!["/bin/sleep", "9"]);
        assert!(parse_args(&[
            "--sandbox-id".into(),
            "sbx_01234567".into(),
            "--receipt".into(),
            "/r".into(),
            "--ctrl".into(),
        ])
        .is_err());
        assert!(parse_args(&[
            "--sandbox-id".into(),
            "sbx_01234567".into(),
            "--receipt".into(),
            "/r".into(),
            "--surprise".into(),
            "--".into(),
            "/bin/true".into(),
        ])
        .is_err());
        assert!(parse_args(&[
            "--sandbox-id".into(),
            "sbx_01234567".into(),
            "--receipt".into(),
            "/r".into(),
            "--ctrl".into(),
            "relative.ctrl".into(),
            "--".into(),
            "/bin/true".into(),
        ])
        .is_err());
    }

    #[test]
    fn ctrl_file_missing_keeps_natural_exit_observation() {
        let _g = crate::TEST_CHILD_REAP_LOCK.lock().unwrap();
        let args = base_args(
            Some("/definitely/not-present-lsguest.ctrl".to_string()),
            false,
            &["/bin/true"],
        );
        let (_pid, outcome, sup) = run_child(&args).unwrap();
        match outcome {
            RunOutcome::Child(ObservedExit::Exited(0)) => {}
            other => panic!("expected natural Exited(0), observed {other:?}"),
        }
        assert_eq!(sup.mode, Supervision::Direct, "no nested request: direct");
        assert!(!sup.residual_risk);
    }

    #[test]
    fn ctrl_cancel_kills_child_and_reports_signaled_not_exit() {
        let _g = crate::TEST_CHILD_REAP_LOCK.lock().unwrap();
        let ctrl_path = std::env::temp_dir()
            .join(format!("lsguest-init-cancel-{}.ctrl", std::process::id()))
            .to_string_lossy()
            .to_string();
        std::fs::write(&ctrl_path, "cancel\n").unwrap();
        let args = base_args(Some(ctrl_path.clone()), false, &["/bin/sleep", "30"]);
        let (_pid, outcome, sup) = run_child(&args).unwrap();
        let _ = std::fs::remove_file(&ctrl_path);
        match outcome {
            RunOutcome::Child(ObservedExit::Signaled(9)) => {}
            other => panic!("expected observed Signaled(9), got {other:?}"),
        }
        // Direct mode + ordered kill: the escape tail is honestly unknown.
        assert_eq!(sup.mode, Supervision::Direct);
        assert!(sup.residual_risk, "kill without adoption authority must report residual risk");
    }

    #[test]
    fn nested_pidns_request_degrades_and_payload_runs() {
        // v2.1 contract: a denied unshare is a DOWNGRADE, never a fabricated
        // spawn failure. Whatever this host permits, the payload runs and the
        // receipt names the real mode.
        let _g = crate::TEST_CHILD_REAP_LOCK.lock().unwrap();
        let args = base_args(None, true, &["/bin/true"]);
        let (_pid, outcome, sup) = run_child(&args).expect("payload runs under any mode");
        match outcome {
            RunOutcome::Child(ObservedExit::Exited(0)) => {}
            other => panic!("unexpected nested run outcome {other:?}"),
        }
        if sup.mode != Supervision::NestedPidns {
            // denial path (rootless CI / runc exec MINIMAL_CAPS): a clean
            // natural exit must not lie about residuals.
            assert!(
                !sup.residual_risk,
                "clean exit under {:?} must not report residual risk",
                sup.mode
            );
        }
    }

    #[test]
    fn proc_fidelity_self_check_accepts_own_linux_proc() {
        // The gate must at least accept the REAL /proc when it describes
        // this very process (container/UML namespaces keep this true; a
        // host-rbind /proc lying about pids is what the gate catches).
        assert!(proc_is_faithful(std::process::id() as i32));
        assert!(!proc_is_faithful(-1));
    }

    #[test]
    fn supervision_mode_degrades_in_exact_order() {
        assert_eq!(supervision_mode(false, 0, 0), Supervision::Direct);
        assert_eq!(supervision_mode(true, 0, -1), Supervision::NestedPidns);
        assert_eq!(supervision_mode(true, -1, 0), Supervision::Subreaper);
        assert_eq!(supervision_mode(true, -1, -1), Supervision::Direct);
        // denial + subreaper success degrades, never pretends isolation
        assert_ne!(supervision_mode(true, -1, 0), Supervision::NestedPidns);
    }
}
