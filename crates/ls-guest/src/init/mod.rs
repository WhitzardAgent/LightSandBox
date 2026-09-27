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
pub fn render_receipt(sandbox_id: &str, child_pid: i32, exit: ObservedExit) -> String {
    let termination = match exit {
        ObservedExit::Exited(code) => format!("{{\"kind\":\"EXITED\",\"exit_code\":{code}}}"),
        ObservedExit::Signaled(sig) => format!("{{\"kind\":\"SIGNALED\",\"signal\":{sig}}}"),
    };
    format!(
        "{{\"version\":1,\"sandbox_id\":\"{sandbox_id}\",\"child_pid\":{child_pid},\
\"termination\":{termination},\"source\":\"init_waitpid\"}}\n"
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
            sa_handler: forward_handler as usize,
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

/// Spawn the entrypoint as a direct child (argv only — no shell), then run
/// the reap/forward loop until the child exits. stdio inside the guest is
/// inherited exactly as the runtime arranged it for PID 1. Returns the
/// child pid and the outcome; the caller writes the receipt and exits.
///
/// T11 v2 semantics (additive, only when the flags are present):
/// * `nested_pidns`: `unshare(CLONE_NEWPID)` BEFORE the spawn — the payload
///   becomes PID 1 of a fresh nested namespace. Its entire progeny (incl.
///   doublefork/setsid escapees) lives and dies inside that namespace.
/// * `ctrl`: the wait loop polls the cancel control file every 20 ms with
///   WNOHANG; a `cancel` prefix SIGKILLs OUR child (the namespace init when
///   nested — kernel then wipes that whole world). Receipt stays the
///   honest wait observation (SIGNALED 9); host-side attribution of who
///   ordered the kill is the host's own affair, never guessed here.
pub fn run_child(args: &InitArgs) -> Result<(i32, RunOutcome), SpawnFailure> {
    install_forward_handlers();
    // Nested namespace first: an unshare failure is a PRE-payload
    // infrastructure truth — reported as spawn failure (payload never ran).
    if args.nested_pidns {
        let rc = unsafe { sys::unshare(sys::CLONE_NEWPID) };
        if rc != 0 {
            return Err(SpawnFailure {
                argv0: args.argv[0].clone(),
                detail: format!("unshare(CLONE_NEWPID) failed: errno {}", sys::errno()),
            });
        }
    }
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

    match &args.ctrl {
        None => Ok(reap_blocking(child_pid)),
        Some(ctrl) => Ok(reap_with_ctrl(child_pid, ctrl)),
    }
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
fn reap_with_ctrl(child_pid: i32, ctrl: &str) -> (i32, RunOutcome) {
    let mut kill_sent = false;
    loop {
        let mut status: sys::c_int = 0;
        let reaped = unsafe { sys::waitpid(-1, &mut status, sys::WNOHANG) };
        if reaped == child_pid {
            return child_outcome(child_pid, status);
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
                );
            } else {
                return (
                    child_pid,
                    RunOutcome::WaitInvariantBroken(format!("waitpid(-1) failed with errno {err}")),
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
                        );
                    }
                }
                _ => {} // missing/empty/short-lived read error: keep polling
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
        Ok((child_pid, RunOutcome::Child(exit))) => {
            let body = render_receipt(&parsed.sandbox_id, child_pid, exit);
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
        Ok((_, RunOutcome::WaitInvariantBroken(detail))) => {
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

    #[test]
    fn receipt_rendering_is_deterministic_and_typed() {
        let body = render_receipt("sbx_01234567", 4242, ObservedExit::Exited(7));
        assert!(body.contains("\"sandbox_id\":\"sbx_01234567\""));
        assert!(body.contains("\"child_pid\":4242"));
        assert!(body.contains("\"termination\":{\"kind\":\"EXITED\",\"exit_code\":7}"));
        assert!(body.contains("\"source\":\"init_waitpid\""));
        let sig = render_receipt("sbx_01234567", 4242, ObservedExit::Signaled(15));
        assert!(sig.contains("\"termination\":{\"kind\":\"SIGNALED\",\"signal\":15}"));
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
        let body = render_receipt("sbx_01234567", 1, ObservedExit::Exited(3));
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
}
