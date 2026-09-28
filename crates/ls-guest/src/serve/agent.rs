//! The `lsguest serve` control loop.
//!
//! Security position (ADR 0002 §8/§13): the agent is a control-channel peer.
//! It TRUSTS the supervisor at the other end of its fd; there is no
//! authentication in v1 because the channel is a private fd handed down by
//! the guest launch path, not a network socket. The agent therefore never
//! touches guest state it did not create: the only kills it ever issues go
//! to direct children it spawned itself ([`exec::Active`]), and a protocol
//! violation closes the channel without guessing, without console output,
//! and without taking the guest down (the process just stops serving).
//!
//! Concurrency model: exactly ONE active exec plus an asynchronous control
//! plane. A reader thread decodes frames into a bounded queue; the control
//! loop below ticks every 20 ms to poll the child and enforce the timeout,
//! so CANCEL / SIGNAL / HEALTH_PING / SHUTDOWN frames are processed while an
//! exec runs; a second EXEC_START while busy is EXEC_REJECTED "busy". All
//! outbound frames (control replies and output chunks alike) flow through
//! ONE bounded `SyncChannel` into a single writer path, keeping wire order
//! and bounding memory at ~capacity × 64 KiB.

use std::fmt;
use std::io::{Read, Write};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use crate::serve::exec::{self, Active, Cause, OutItem};
use crate::serve::protocol::{self, Body, Frame, FrameRead, Violation};

/// Usage/argument contract violation (same convention as ls-guest init).
pub const EXIT_USAGE: i32 = 2;
/// The channel could not be adopted, or serving ended on a protocol
/// violation or I/O error. SHUTDOWN and clean EOF are exit 0.
pub const EXIT_FAILURE: i32 = 1;

/// Default control-loop tick: timeout-enforcement granularity and the poll
/// cadence of an active exec (round-05 directive: e.g. 20 ms).
pub const TICK: Duration = Duration::from_millis(20);

/// Outbound queue capacity in frames. Chunks are ≤ 64 KiB each, so the
/// in-flight output is bounded at ~1 MiB.
pub const OUT_CAPACITY: usize = 16;

/// Inbound decoded-frame queue depth; the reader thread blocks beyond it,
/// which is the only inbound backpressure a byte stream offers.
pub const IN_CAPACITY: usize = 8;

/// The guest control agent. Configuration is deliberately minimal and has
/// sane v1 defaults.
#[derive(Debug, Clone)]
pub struct Agent {
    tick: Duration,
    out_capacity: usize,
    in_capacity: usize,
}

impl Default for Agent {
    fn default() -> Self {
        Agent {
            tick: TICK,
            out_capacity: OUT_CAPACITY,
            in_capacity: IN_CAPACITY,
        }
    }
}

impl Agent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve the channel until SHUTDOWN, peer EOF, or a fail-closed drop.
    /// `reader`/`writer` are the two directions of the same transport (a
    /// `try_clone`d socket in tests, a mirrored serial fd in production).
    /// Never panics and never exits the process; on return both directions
    /// are closed and any exec child is reaped.
    ///
    /// Liveness note: if the peer keeps the transport open but stops
    /// draining it, the single writer path parks inside `write_all` —
    /// deliberate backpressure (bounded memory), so a wedged-but-open peer
    /// can park serving. The serial peer is the trusted supervisor; a peer
    /// that CLOSES the transport instead always unwinds promptly (write
    /// errors end the writer, EOF ends the reader).
    pub fn run<R, W>(&self, reader: R, writer: W) -> RunEnd
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (out_tx, out_rx) = sync_channel::<OutItem>(self.out_capacity);
        let mut writer_thread = Some(std::thread::spawn(move || exec::run_writer(writer, out_rx)));
        let (in_tx, in_rx) = sync_channel::<ReadEvent>(self.in_capacity);
        // The reader thread is detached on purpose: after SHUTDOWN it may
        // still be parked in `read` until the supervisor closes its end (the
        // process exits anyway). On every other RunEnd the thread itself
        // delivered the terminal event and is already gone.
        std::thread::spawn(move || read_loop(reader, in_tx));

        let mut active: Option<Active> = None;
        loop {
            match in_rx.recv_timeout(self.tick) {
                Ok(ReadEvent::Frame(frame)) => match self.dispatch(frame, &mut active, &out_tx) {
                    Dispatch::Continue => {}
                    Dispatch::Shutdown => {
                        // SHUTDOWN_DONE was queued as Final: join the writer
                        // FIRST so the flush is observed before the kill,
                        // then terminate and reap the active exec child.
                        if let Some(w) = writer_thread.take() {
                            let _ = w.join();
                        }
                        if let Some(a) = active.take() {
                            a.terminate();
                        }
                        return RunEnd::Shutdown;
                    }
                    Dispatch::Drop(v) => {
                        end_channel(&mut active, out_tx, writer_thread.take().unwrap());
                        return RunEnd::Dropped(v);
                    }
                },
                Ok(ReadEvent::Eof) => {
                    end_channel(&mut active, out_tx, writer_thread.take().unwrap());
                    return RunEnd::PeerEof;
                }
                Ok(ReadEvent::Violation(v)) => {
                    end_channel(&mut active, out_tx, writer_thread.take().unwrap());
                    return RunEnd::Dropped(v);
                }
                Ok(ReadEvent::Io(e)) => {
                    end_channel(&mut active, out_tx, writer_thread.take().unwrap());
                    return RunEnd::IoFailure(e);
                }
                Err(RecvTimeoutError::Timeout) => self.tick_exec(&mut active, &out_tx),
                Err(RecvTimeoutError::Disconnected) => {
                    // The reader thread died without delivering a terminal
                    // event: a harness bug, surfaced, never swallowed.
                    end_channel(&mut active, out_tx, writer_thread.take().unwrap());
                    return RunEnd::IoFailure("frame reader thread disconnected".into());
                }
            }
        }
    }

    /// Handle one host frame. Any answer is queued behind earlier frames.
    fn dispatch(
        &self,
        frame: Frame,
        active: &mut Option<Active>,
        out_tx: &SyncSender<OutItem>,
    ) -> Dispatch {
        let id = frame.request_id;
        match frame.body {
            Body::HealthPing => {
                send(
                    out_tx,
                    Frame::new(
                        id,
                        Body::HealthPong {
                            proto_ver: protocol::PROTO_VERSION,
                            target: target_triple(),
                        },
                    ),
                );
            }
            Body::ExecStart(spec) => {
                if active.is_some() {
                    // Exactly ONE active exec: reject, never queue.
                    reject(out_tx, id, "busy");
                } else {
                    match Active::spawn(&spec, id) {
                        Ok(mut a) => {
                            // ACCEPTED precedes any chunk on the wire: pumps
                            // start only after it is queued.
                            send(out_tx, Frame::new(id, Body::ExecAccepted));
                            a.start_pumps(out_tx.clone());
                            *active = Some(a);
                        }
                        // v1: a spawn failure is FINISHED kind 4, not a
                        // REJECTED (the request was never accepted).
                        Err(reason) => {
                            send(
                                out_tx,
                                Frame::new(
                                    id,
                                    Body::Finished(protocol::Finished::SpawnFailed(reason)),
                                ),
                            );
                        }
                    }
                }
            }
            Body::ExecCancel => {
                // Cancel of a stale/unknown id is ignored (idempotent): only
                // the ACTIVE request's cancel terminates its child.
                if active.as_ref().map(|a| a.request_id) == Some(id) {
                    let mut a = active.take().unwrap();
                    a.set_cause(Cause::Cancelled);
                    let finished = a.finish();
                    send(out_tx, Frame::new(id, Body::Finished(finished)));
                }
            }
            Body::Signal { signo } => {
                if signo != 9 {
                    // v1 supports SIGKILL to the exec child only.
                    reject(out_tx, id, "signal-not-supported-v1");
                } else if active.as_ref().map(|a| a.request_id) == Some(id) {
                    let mut a = active.take().unwrap();
                    a.set_cause(Cause::KilledBySignal(9));
                    let finished = a.finish();
                    send(out_tx, Frame::new(id, Body::Finished(finished)));
                }
            }
            Body::Shutdown => {
                send_final(out_tx, Frame::new(id, Body::ShutdownDone));
                return Dispatch::Shutdown;
            }
            Body::ExecStdin(_) => {
                // v1 reserved seam: stdin frames are well-formed but
                // semantically unsupported.
                reject(out_tx, id, "stdin-not-supported-v1");
            }
            Body::ReservedFileApi(_) => {
                reject(out_tx, id, "reserved-type");
            }
            // A guest→host frame arriving at the agent is a direction
            // confusion: fail-closed, drop without an answer. The wire path
            // filters these in `read_loop` already; this arm is in-loop
            // defence for any future caller of `dispatch` that hands it raw
            // frames.
            Body::HealthPong { .. }
            | Body::ExecAccepted
            | Body::ExecRejected { .. }
            | Body::StdoutChunk(_)
            | Body::StderrChunk(_)
            | Body::Finished(_)
            | Body::ShutdownDone => {
                return Dispatch::Drop(Violation::WrongDirection(protocol::body_type(
                    &frame.body,
                )))
            }
        }
        Dispatch::Continue
    }

    /// Poll the active exec between frame events: report natural exit, then
    /// enforce the timeout. FINISHED is queued only after the pump threads
    /// joined, so no chunk can overtake it on the wire.
    fn tick_exec(&self, active: &mut Option<Active>, out_tx: &SyncSender<OutItem>) {
        let Some(a) = active.as_mut() else { return };
        match a.try_wait() {
            Ok(Some(_)) => {
                let a = active.take().unwrap();
                let id = a.request_id;
                let finished = a.finish();
                send(out_tx, Frame::new(id, Body::Finished(finished)));
            }
            Ok(None) => {
                if a.timed_out(Instant::now()) {
                    let mut a = active.take().unwrap();
                    let id = a.request_id;
                    a.set_cause(Cause::TimedOut);
                    let finished = a.finish();
                    send(out_tx, Frame::new(id, Body::Finished(finished)));
                }
            }
            Err(e) => {
                // try_wait breaking is an OS-level anomaly: reap best-effort
                // and report it honestly as spawn-failed-class trouble,
                // never as a guessed exit code.
                let a = active.take().unwrap();
                let id = a.request_id;
                let finished = a.fail_wait(format!("try_wait failed: {e}"));
                send(out_tx, Frame::new(id, Body::Finished(finished)));
            }
        }
    }
}

/// Outcome of dispatching one frame.
enum Dispatch {
    Continue,
    /// SHUTDOWN_DONE queued as `Final`; the loop flushes and kills next.
    Shutdown,
    /// Fail-closed drop with no answer (direction confusion).
    Drop(Violation),
}

/// Why `Agent::run` returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// Clean SHUTDOWN handshake: SHUTDOWN_DONE flushed, active child killed.
    Shutdown,
    /// The peer hung up at a frame boundary without SHUTDOWN.
    PeerEof,
    /// Fail-closed drop: a violation closed the channel without any answer.
    Dropped(Violation),
    /// An OS-level I/O failure on the channel.
    IoFailure(String),
}

impl fmt::Display for RunEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunEnd::Shutdown => write!(f, "shutdown handshake complete"),
            RunEnd::PeerEof => write!(f, "peer closed the channel"),
            RunEnd::Dropped(v) => write!(f, "protocol violation, channel dropped: {v}"),
            RunEnd::IoFailure(e) => write!(f, "channel I/O failure: {e}"),
        }
    }
}

/// Best-effort target triple for HEALTH_PONG, composed from std build
/// constants: e.g. `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`.
pub fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    let vendor = if cfg!(target_vendor = "apple") {
        "apple"
    } else if cfg!(target_vendor = "pc") {
        "pc"
    } else {
        "unknown"
    };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let env = if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "gnu") {
        "gnu"
    } else {
        ""
    };
    if env.is_empty() {
        format!("{arch}-{vendor}-{os}")
    } else {
        format!("{arch}-{vendor}-{os}-{env}")
    }
}

fn send(out_tx: &SyncSender<OutItem>, frame: Frame) {
    // If the writer is gone (peer dead), the frame is simply lost — the
    // channel is dead anyway; nothing else to do and nothing to log.
    let _ = out_tx.send(OutItem::Frame(frame));
}

fn send_final(out_tx: &SyncSender<OutItem>, frame: Frame) {
    let _ = out_tx.send(OutItem::Final(frame));
}

fn reject(out_tx: &SyncSender<OutItem>, id: u32, reason: &str) {
    send(
        out_tx,
        Frame::new(
            id,
            Body::ExecRejected {
                reason: reason.to_string(),
            },
        ),
    );
}

/// Terminal cleanup for a dropped/EOF/failed channel: kill and reap the
/// active child (never leave guest processes behind), drop the outbound
/// queue, and join the writer thread so both directions are closed on
/// return. Nothing is written to the guest console.
fn end_channel(
    active: &mut Option<Active>,
    out_tx: SyncSender<OutItem>,
    writer_thread: std::thread::JoinHandle<std::io::Result<()>>,
) {
    if let Some(a) = active.take() {
        a.terminate();
    }
    drop(out_tx);
    let _ = writer_thread.join();
}

/// What the reader thread reports to the control loop.
enum ReadEvent {
    Frame(Frame),
    Eof,
    Violation(Violation),
    Io(String),
}

/// Decode frames forever; deliver each frame or one terminal event, then
/// stop. The reader side of the channel is dropped when this thread exits —
/// which is what closes the host's read on every fail-closed drop, so any
/// condition that must kill the channel has to be terminal HERE (the
/// control loop cannot close a reader it does not own).
fn read_loop<R: Read>(mut reader: R, tx: SyncSender<ReadEvent>) {
    loop {
        let event = match protocol::read_frame(&mut reader) {
            Ok(FrameRead::Frame(f)) => {
                let t = protocol::body_type(&f.body);
                if protocol::is_host_to_guest(t) {
                    ReadEvent::Frame(f)
                } else {
                    // A guest→host frame arriving at the agent is a
                    // direction confusion: fail-closed, and terminal here so
                    // the read half closes (see the doc above).
                    ReadEvent::Violation(Violation::WrongDirection(t))
                }
            }
            Ok(FrameRead::Eof) => ReadEvent::Eof,
            Ok(FrameRead::Violation(v)) => ReadEvent::Violation(v),
            Err(e) => ReadEvent::Io(format!("{e}")),
        };
        let terminal = !matches!(event, ReadEvent::Frame(_));
        if tx.send(event).is_err() {
            return; // control loop gone (SHUTDOWN path): just stop reading
        }
        if terminal {
            return;
        }
    }
}

// ------------------------------------------------------------------ CLI glue

/// Verified arguments of `lsguest serve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeArgs {
    pub fd: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeArgError {
    BadShape(&'static str),
    BadFd(&'static str),
}

impl fmt::Display for ServeArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeArgError::BadShape(what) => {
                write!(f, "expected `serve --fd <N>`: {what}")
            }
            ServeArgError::BadFd(why) => write!(f, "invalid fd: {why}"),
        }
    }
}

impl std::error::Error for ServeArgError {}

/// Parse exactly `serve --fd <N>`. argv form only; there is deliberately no
/// `--dev-socket` (or any other) flag: the channel is fd-only.
pub fn parse_serve_args(argv: &[String]) -> Result<ServeArgs, ServeArgError> {
    let mut it = argv.iter();
    match it.next().map(String::as_str) {
        Some("serve") => {}
        _ => return Err(ServeArgError::BadShape("missing `serve` subcommand")),
    }
    match it.next().map(String::as_str) {
        Some("--fd") => {}
        Some(other @ "--dev-socket") | Some(other) => {
            return Err(ServeArgError::BadShape(match other {
                "--dev-socket" => "--dev-socket is not supported; the channel is fd-only",
                _ => "expected --fd <N>",
            }))
        }
        None => return Err(ServeArgError::BadShape("expected --fd <N>")),
    }
    let value = match it.next() {
        Some(v) => v,
        None => return Err(ServeArgError::BadShape("missing value after --fd")),
    };
    if let Some(extra) = it.next() {
        return Err(ServeArgError::BadShape(match extra.as_str() {
            "--dev-socket" => "--dev-socket is not supported; the channel is fd-only",
            _ => "unexpected extra argument",
        }));
    }
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ServeArgError::BadFd("must be a non-negative integer"));
    }
    match value.parse::<i32>() {
        Ok(fd) => Ok(ServeArgs { fd }),
        Err(_) => Err(ServeArgError::BadFd("out of range")),
    }
}

/// Adopt an already-open channel fd without `unsafe`: open the process-local
/// fd mirror (`/proc/self/fd`, falling back to `/dev/fd`) for read+write,
/// which duplicates the descriptor the way `dup(2)` would. This requires the
/// descriptor to reference an openable node — true for the production guest
/// serial line (a character device); socketpair test fds bypass this helper
/// and reach `Agent::run` via `UnixStream::try_clone` directly.
pub fn open_channel_fd(fd: i32) -> std::io::Result<std::fs::File> {
    if fd < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid channel fd {fd}"),
        ));
    }
    let proc = format!("/proc/self/fd/{fd}");
    let dev = format!("/dev/fd/{fd}");
    match open_rw(&proc) {
        Ok(f) => Ok(f),
        Err(proc_err) => open_rw(&dev).map_err(|dev_err| {
            std::io::Error::new(
                dev_err.kind(),
                format!("cannot adopt channel fd {fd}: {proc}: {proc_err}; {dev}: {dev_err}"),
            )
        }),
    }
}

fn open_rw(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::File::options().read(true).write(true).open(path)
}

/// Entry point of the `lsguest` binary: parse args, adopt the channel fd,
/// serve. All behaviour lives in the lib so tests need no agent child
/// process; this is only the argv/OS handshake. Prints to stderr only BEFORE
/// the channel is adopted — never while serving.
pub fn main_entry(argv: Vec<String>) -> i32 {
    let rest: &[String] = if argv.is_empty() { &[] } else { &argv[1..] };
    let args = match parse_serve_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lsguest: usage error: {e}");
            return EXIT_USAGE;
        }
    };
    let channel = match open_channel_fd(args.fd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lsguest: {e}");
            return EXIT_FAILURE;
        }
    };
    let writer = match channel.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("lsguest: cannot clone channel fd {}: {e}", args.fd);
            return EXIT_FAILURE;
        }
    };
    match Agent::new().run(channel, writer) {
        RunEnd::Shutdown | RunEnd::PeerEof => 0,
        RunEnd::Dropped(_) | RunEnd::IoFailure(_) => EXIT_FAILURE,
    }
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::protocol::{ty, ExecStart, Finished};
    use std::os::unix::net::UnixStream;
    use std::thread::JoinHandle;

    /// Live agent at one end of a socketpair; the harness is the supervisor.
    struct Harness {
        sock: UnixStream,
        agent: Option<JoinHandle<RunEnd>>,
    }

    fn harness() -> Harness {
        let (host, guest) = UnixStream::pair().unwrap();
        let reader = guest.try_clone().unwrap();
        let agent = std::thread::spawn(move || Agent::new().run(reader, guest));
        let sock = host;
        sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        Harness {
            sock,
            agent: Some(agent),
        }
    }

    impl Harness {
        fn send(&mut self, frame: &Frame) {
            protocol::write_frame(&mut self.sock, frame).unwrap();
        }

        fn raw(&mut self, bytes: &[u8]) {
            self.sock.write_all(bytes).unwrap();
        }

        fn recv(&mut self) -> Frame {
            match protocol::read_frame(&mut self.sock).unwrap() {
                FrameRead::Frame(f) => f,
                other => panic!("expected a frame, got {other:?}"),
            }
        }

        /// Skip any chunk frames until a FINISHED arrives (chunks may
        /// legitimately interleave with everything else).
        fn recv_finished(&mut self) -> (u32, Finished) {
            loop {
                let f = self.recv();
                if let Body::Finished(done) = f.body {
                    return (f.request_id, done);
                }
            }
        }

        /// The agent closed its end without writing a single response byte:
        /// EOF at a frame boundary. Any answer (even one byte) would
        /// surface here as a frame or a mid-frame truncation instead.
        fn expect_eof(&mut self) {
            match protocol::read_frame(&mut self.sock).unwrap() {
                FrameRead::Eof => {}
                other => panic!("expected silent channel close (EOF), got {other:?}"),
            }
        }

        /// Collect all frames for `id` until its FINISHED, returning the
        /// concatenated stdout and stderr chunks in arrival order plus the
        /// FINISHED kind. Asserts every session frame carries `id`.
        fn session(&mut self, id: u32) -> (Vec<u8>, Vec<u8>, Finished) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            loop {
                let f = self.recv();
                assert_eq!(f.request_id, id, "session frame with foreign id");
                match f.body {
                    Body::ExecAccepted => {}
                    Body::StdoutChunk(b) => out.extend_from_slice(&b),
                    Body::StderrChunk(b) => err.extend_from_slice(&b),
                    Body::Finished(done) => return (out, err, done),
                    other => panic!("unexpected session frame: {other:?}"),
                }
            }
        }

        fn join_agent(mut self, within: Duration) -> RunEnd {
            let agent = self.agent.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || tx.send(agent.join()).unwrap());
            rx.recv_timeout(within)
                .unwrap_or_else(|_| panic!("agent thread did not terminate within {within:?}"))
                .expect("agent thread panicked")
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            // Kill the channel so a failing test never parks the agent.
            let _ = self.sock.shutdown(std::net::Shutdown::Both);
            if let Some(agent) = self.agent.take() {
                let _ = agent.join();
            }
        }
    }

    fn exec_start(id: u32, timeout_ms: u32, argv: &[&[u8]]) -> Frame {
        Frame::new(
            id,
            Body::ExecStart(ExecStart {
                timeout_ms,
                cwd: b"/".to_vec(),
                argv: argv.iter().map(|a| a.to_vec()).collect(),
            }),
        )
    }

    fn sh(id: u32, timeout_ms: u32, script: &str) -> Frame {
        exec_start(id, timeout_ms, &[b"/bin/sh", b"-c", script.as_bytes()])
    }

    /// Raw header bytes (for crafting hostile frames).
    fn raw_frame(msg_type: u16, request_id: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&protocol::MAGIC);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&msg_type.to_be_bytes());
        b.extend_from_slice(&request_id.to_be_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        b.extend_from_slice(payload);
        b
    }

    // ------------------------------------------------------------ CLI / fd

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn serve_args_parse_exactly_serve_fd() {
        assert_eq!(
            parse_serve_args(&argv(&["serve", "--fd", "7"])).unwrap(),
            ServeArgs { fd: 7 }
        );
        assert!(parse_serve_args(&argv(&[])).is_err());
        assert!(parse_serve_args(&argv(&["serve"])).is_err());
        assert!(parse_serve_args(&argv(&["serve", "--fd"])).is_err());
        assert!(parse_serve_args(&argv(&["--fd", "7"])).is_err());
        assert!(parse_serve_args(&argv(&["serve", "--fd", "-1"])).is_err());
        assert!(parse_serve_args(&argv(&["serve", "--fd", "1x"])).is_err());
        assert!(parse_serve_args(&argv(&["serve", "--fd", "99999999999"])).is_err());
        assert!(parse_serve_args(&argv(&["serve", "--fd", "7", "extra"])).is_err());
    }

    #[test]
    fn dev_socket_flag_is_explicitly_refused() {
        let err = parse_serve_args(&argv(&["serve", "--dev-socket", "/tmp/x"])).unwrap_err();
        assert!(format!("{err}").contains("--dev-socket"));
        let err2 = parse_serve_args(&argv(&["serve", "--fd", "7", "--dev-socket"])).unwrap_err();
        assert!(format!("{err2}").contains("--dev-socket"));
    }

    #[test]
    fn open_channel_fd_adopts_a_duplex_fd_without_unsafe() {
        use std::os::unix::io::IntoRawFd;
        // A duplex fd obtained through safe std APIs only (the production
        // channel fd is an O_RDWR serial device; a plain file opened
        // read+write is the portable test double — a one-directional pipe
        // fd would fail the mirror's mode check, unlike the real channel).
        let path = std::env::temp_dir().join(format!(
            "lsbx-fdmirror-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(b"channel").unwrap();
        file.flush().unwrap();
        let raw = file.into_raw_fd();
        let mut adopted = open_channel_fd(raw).unwrap();
        // The mirror is an independent open file description of the same
        // channel node: writing through it lands in the same object.
        adopted.write_all(b"-mirror").unwrap();
        adopted.flush().unwrap();
        drop(adopted);
        let mut check = std::fs::File::open(&path).unwrap();
        let mut content = String::new();
        check.read_to_string(&mut content).unwrap();
        // Bytes written through the mirror landed in the same channel node.
        // (Where they land differs by platform: macOS /dev/fd shares the
        // file offset like dup; Linux /proc/self/fd opens independently.)
        assert!(content.contains("-mirror"), "unexpected content: {content:?}");
        let _ = std::fs::remove_file(&path);
        // `raw` itself is left to the OS at test exit: reclaiming it would
        // need from_raw_fd, i.e. unsafe, which this module forbids.
    }

    #[test]
    fn open_channel_fd_rejects_unknown_fd() {
        assert!(open_channel_fd(1_073_741_821).is_err());
    }

    #[test]
    fn main_entry_usage_error_is_exit_2() {
        assert_eq!(main_entry(argv(&["lsguest"])), EXIT_USAGE);
        assert_eq!(main_entry(argv(&["lsguest", "serve", "--fd", "x"])), EXIT_USAGE);
    }

    #[test]
    fn main_entry_unadoptable_fd_is_exit_1() {
        assert_eq!(
            main_entry(argv(&["lsguest", "serve", "--fd", "1073741821"])),
            EXIT_FAILURE
        );
    }

    // -------------------------------------------------------- 4a: health

    #[test]
    fn health_ping_answers_pong_with_proto_1_and_target() {
        let mut h = harness();
        h.send(&Frame::new(42, Body::HealthPing));
        let f = h.recv();
        assert_eq!(f.request_id, 42);
        match f.body {
            Body::HealthPong { proto_ver, target } => {
                assert_eq!(proto_ver, 1);
                assert!(!target.is_empty());
                assert!(target.contains('-'), "target should look like a triple: {target}");
            }
            other => panic!("expected HEALTH_PONG, got {other:?}"),
        }
    }

    // ------------------------------------------------------ 4b/c/d: exec

    #[test]
    fn exec_echo_streams_stdout_and_exit_zero() {
        let mut h = harness();
        h.send(&sh(1001, 0, "echo hello"));
        let (out, _err, done) = h.session(1001);
        assert_eq!(String::from_utf8_lossy(&out), "hello\n");
        assert_eq!(done, Finished::Exited(0));
    }

    #[test]
    fn exec_reports_real_exit_code_and_stderr() {
        let mut h = harness();
        h.send(&sh(1002, 0, "exit 7"));
        let (_o, _e, done) = h.session(1002);
        assert_eq!(done, Finished::Exited(7));

        h.send(&sh(1003, 0, "echo e >&2"));
        let (_o, err, done) = h.session(1003);
        assert_eq!(String::from_utf8_lossy(&err), "e\n");
        assert_eq!(done, Finished::Exited(0));
    }

    #[test]
    fn exec_streams_binary_output_intact() {
        let mut h = harness();
        // Octal escapes are the POSIX-portable form of printf "\xff\x00".
        h.send(&sh(1004, 0, "printf '\\377\\000'"));
        let (out, _err, done) = h.session(1004);
        assert_eq!(out, vec![0xff, 0x00]);
        assert_eq!(done, Finished::Exited(0));
    }

    #[test]
    fn exec_large_output_survives_backpressure() {
        let mut h = harness();
        // 5 MiB ≫ queue capacity × 64 KiB: proves no byte loss and no
        // deadlock under OS-level backpressure.
        h.send(&sh(1005, 0, "dd if=/dev/zero bs=1024 count=5120 2>/dev/null"));
        let (out, _err, done) = h.session(1005);
        assert_eq!(out.len(), 5 * 1024 * 1024);
        assert!(out.iter().all(|&b| b == 0));
        assert_eq!(done, Finished::Exited(0));
    }

    #[test]
    fn exec_respects_cwd() {
        let mut h = harness();
        let mut req = sh(1006, 0, "pwd");
        if let Body::ExecStart(spec) = &mut req.body {
            spec.cwd = b"/tmp".to_vec();
        }
        h.send(&req);
        let (out, _err, done) = h.session(1006);
        let path = String::from_utf8_lossy(&out).trim().to_string();
        assert!(path == "/tmp" || path == "/private/tmp", "unexpected pwd: {path}");
        assert_eq!(done, Finished::Exited(0));
    }

    // ----------------------------------------------------- 4e: spawn fail

    #[test]
    fn exec_spawn_failure_is_finished_kind_4_then_agent_keeps_serving() {
        let mut h = harness();
        h.send(&exec_start(2001, 0, &[b"/nonexistent/lsbx-void"]));
        let (id, done) = h.recv_finished();
        assert_eq!(id, 2001);
        match done {
            Finished::SpawnFailed(detail) => assert!(!detail.is_empty()),
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
        h.send(&sh(2002, 0, "echo still-alive"));
        let (out, _e, done) = h.session(2002);
        assert_eq!(String::from_utf8_lossy(&out), "still-alive\n");
        assert_eq!(done, Finished::Exited(0));
    }

    // --------------------------------------------------------- 4f: busy

    #[test]
    fn second_exec_while_busy_is_rejected_and_health_still_answers() {
        let mut h = harness();
        h.send(&exec_start(3001, 0, &[b"/bin/sleep", b"5"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);

        h.send(&sh(3002, 0, "echo no"));
        let f = h.recv();
        assert_eq!(f.request_id, 3002);
        assert_eq!(
            f.body,
            Body::ExecRejected {
                reason: "busy".into()
            }
        );

        h.send(&Frame::new(3003, Body::HealthPing));
        let f = h.recv();
        assert_eq!(f.request_id, 3003);
        assert!(matches!(f.body, Body::HealthPong { proto_ver: 1, .. }));

        h.send(&Frame::new(3001, Body::ExecCancel));
        let (id, done) = h.recv_finished();
        assert_eq!((id, done), (3001, Finished::Cancelled));
    }

    // ---------------------------------------------- 4g: cancel then serve

    #[test]
    fn cancel_kills_active_exec_and_agent_keeps_serving() {
        let mut h = harness();
        h.send(&exec_start(4001, 0, &[b"/bin/sleep", b"30"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);
        let started = Instant::now();

        h.send(&Frame::new(4001, Body::ExecCancel));
        let (id, done) = h.recv_finished();
        assert_eq!((id, done), (4001, Finished::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel took too long: {:?}",
            started.elapsed()
        );

        // §12: the agent MUST keep serving after a cancel.
        h.send(&sh(4002, 0, "echo after-cancel"));
        let (out, _e, done) = h.session(4002);
        assert_eq!(String::from_utf8_lossy(&out), "after-cancel\n");
        assert_eq!(done, Finished::Exited(0));
    }

    // -------------------------------------------------------- 4h: timeout

    #[test]
    fn timeout_kills_exec_within_grace() {
        let mut h = harness();
        h.send(&exec_start(5001, 200, &[b"/bin/sleep", b"30"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);
        let started = Instant::now();
        let (id, done) = h.recv_finished();
        assert_eq!((id, done), (5001, Finished::Timeout));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout took too long: {:?}",
            started.elapsed()
        );
        h.send(&sh(5002, 0, "echo after-timeout"));
        let (_o, _e, done) = h.session(5002);
        assert_eq!(done, Finished::Exited(0));
    }

    // --------------------------------------------------------- 4i: signal

    #[test]
    fn signal_rejects_non_kill_and_signo_9_kills() {
        let mut h = harness();
        h.send(&exec_start(6001, 0, &[b"/bin/sleep", b"30"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);

        h.send(&Frame::new(6001, Body::Signal { signo: 15 }));
        let f = h.recv();
        assert_eq!(
            (f.request_id, f.body),
            (
                6001,
                Body::ExecRejected {
                    reason: "signal-not-supported-v1".into()
                }
            )
        );

        h.send(&Frame::new(6001, Body::Signal { signo: 9 }));
        let started = Instant::now();
        let (id, done) = h.recv_finished();
        assert_eq!((id, done), (6001, Finished::Signaled(9)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    // --------------------------------------------- v1 seams: stdin, file

    #[test]
    fn stdin_frame_and_reserved_types_are_rejected_not_dropped() {
        let mut h = harness();
        h.send(&Frame::new(7001, Body::ExecStdin(b"some stdin bytes".to_vec())));
        let f = h.recv();
        assert_eq!(
            (f.request_id, f.body),
            (
                7001,
                Body::ExecRejected {
                    reason: "stdin-not-supported-v1".into()
                }
            )
        );
        for t in [0x0100u16, 0x01ffu16] {
            h.send(&Frame::new(7100, Body::ReservedFileApi(vec![t as u8])));
            let f = h.recv();
            assert_eq!(
                (f.request_id, f.body),
                (
                    7100,
                    Body::ExecRejected {
                        reason: "reserved-type".into()
                    }
                )
            );
        }
        // The channel is alive afterwards.
        h.send(&Frame::new(7002, Body::HealthPing));
        assert!(matches!(h.recv().body, Body::HealthPong { .. }));
    }

    // ------------------------------------------------------- 4j: shutdown

    #[test]
    fn shutdown_flushes_done_kills_child_and_terminates() {
        let mut h = harness();
        h.send(&exec_start(8001, 0, &[b"/bin/sleep", b"30"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);
        let started = Instant::now();

        h.send(&Frame::new(8002, Body::Shutdown));
        let f = h.recv();
        assert_eq!((f.request_id, f.body), (8002, Body::ShutdownDone));
        assert!(started.elapsed() < Duration::from_secs(2));

        let end = h.join_agent(Duration::from_secs(2));
        assert_eq!(end, RunEnd::Shutdown);
    }

    #[test]
    fn shutdown_without_exec_is_clean() {
        let mut h = harness();
        h.send(&Frame::new(8101, Body::Shutdown));
        let f = h.recv();
        assert_eq!((f.request_id, f.body), (8101, Body::ShutdownDone));
        assert_eq!(h.join_agent(Duration::from_secs(2)), RunEnd::Shutdown);
    }

    // ----------------------------------------- fail-closed drop behaviours

    #[test]
    fn half_header_then_close_drops_without_panic() {
        let mut h = harness();
        h.raw(&protocol::MAGIC[..2]); // half a header
        h.sock.shutdown(std::net::Shutdown::Write).unwrap();
        let end = h.join_agent(Duration::from_secs(2));
        assert_eq!(end, RunEnd::Dropped(Violation::Truncated));
        // The agent closed its end too.
    }

    #[test]
    fn wire_violations_drop_the_channel_silently() {
        let cases: Vec<(Vec<u8>, Violation)> = vec![
            // bad magic
            ({
                let mut b = raw_frame(ty::HEALTH_PING, 1, &[]);
                b[1] = b'X';
                b
            }, Violation::BadMagic),
            // wrong version
            ({
                let mut b = raw_frame(ty::HEALTH_PING, 1, &[]);
                b[5] = 9;
                b
            }, Violation::UnsupportedVersion(9)),
            // unknown type
            (raw_frame(0x1234, 1, &[]), Violation::UnknownType(0x1234)),
            // non-empty payload on HEALTH_PING
            (
                raw_frame(ty::HEALTH_PING, 1, &[0]),
                Violation::Malformed("unexpected payload on empty-payload type"),
            ),
            // oversized payload_len (no payload follows; header alone suffices)
            ({
                let mut b = Vec::new();
                b.extend_from_slice(&protocol::MAGIC);
                b.extend_from_slice(&1u16.to_be_bytes());
                b.extend_from_slice(&ty::EXEC_STDIN.to_be_bytes());
                b.extend_from_slice(&7u32.to_be_bytes());
                b.extend_from_slice(&((protocol::MAX_PAYLOAD + 1) as u32).to_be_bytes());
                b
            }, Violation::OversizedPayload((protocol::MAX_PAYLOAD + 1) as u32)),
            // EXEC_START with argv_count 0
            (
                raw_frame(ty::EXEC_START, 1, &[0; 8]),
                Violation::Malformed("argv_count is 0"),
            ),
        ];
        for (frame, want) in cases {
            let mut h = harness();
            h.raw(&frame);
            h.expect_eof();
            let end = h.join_agent(Duration::from_secs(2));
            assert_eq!(end, RunEnd::Dropped(want));
        }
    }

    #[test]
    fn oversized_payload_drops_even_after_a_valid_session() {
        let mut h = harness();
        h.send(&sh(9001, 0, "echo ok"));
        let (_o, _e, done) = h.session(9001);
        assert_eq!(done, Finished::Exited(0));

        let mut b = Vec::new();
        b.extend_from_slice(&protocol::MAGIC);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&ty::HEALTH_PING.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&((protocol::MAX_PAYLOAD + 5) as u32).to_be_bytes());
        h.raw(&b);
        h.expect_eof();
        let end = h.join_agent(Duration::from_secs(2));
        assert_eq!(
            end,
            RunEnd::Dropped(Violation::OversizedPayload(
                (protocol::MAX_PAYLOAD + 5) as u32
            ))
        );
    }

    #[test]
    fn guest_to_host_direction_confusion_drops() {
        let mut h = harness();
        h.raw(&raw_frame(ty::HEALTH_PONG, 1, &[0, 1, 1, b'x']));
        h.expect_eof();
        let end = h.join_agent(Duration::from_secs(2));
        assert_eq!(end, RunEnd::Dropped(Violation::WrongDirection(ty::HEALTH_PONG)));
    }

    #[test]
    fn clean_peer_eof_keeps_exit_clean_and_reaps_child() {
        let mut h = harness();
        h.send(&exec_start(9501, 0, &[b"/bin/sleep", b"30"]));
        assert_eq!(h.recv().body, Body::ExecAccepted);
        h.sock.shutdown(std::net::Shutdown::Write).unwrap();
        let end = h.join_agent(Duration::from_secs(2));
        assert_eq!(end, RunEnd::PeerEof);
    }

    #[test]
    fn target_triple_is_nonempty() {
        let t = target_triple();
        assert!(t.split('-').count() >= 3, "not a triple: {t}");
    }
}
