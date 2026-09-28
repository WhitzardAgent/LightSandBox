//! Guest control protocol v1 — wire format and fail-closed codec.
//!
//! Transport: a single byte-stream fd (in production the guest serial line,
//! in tests a socketpair). Multi-byte integers are big-endian. Every frame is
//! a fixed 16-byte header followed by `payload_len` payload bytes:
//!
//! ```text
//! bytes 0..4    magic ASCII "LSGP"
//! bytes 4..6    u16 protocol_version = 1           (any other version: drop)
//! bytes 6..8    u16 message_type
//! bytes 8..12   u32 request_id                     (host-chosen; the whole
//!               exec session — ACCEPTED/chunks/FINISHED — carries this id)
//! bytes 12..16  u32 payload_len                    (hard cap 1 MiB; a larger
//!               value drops the channel, its payload is never read)
//! bytes 16..    payload
//! ```
//!
//! Message types, host → guest:
//!
//! ```text
//! 0x0001 HEALTH_PING    payload: empty
//! 0x0002 EXEC_START     payload: [u32 timeout_ms (0 = none)]
//!                                [u16 cwd_len][cwd bytes]
//!                                [u16 argv_count]
//!                                per arg: [u32 len][bytes]
//! 0x0003 EXEC_CANCEL    payload: empty; request_id selects the active exec
//! 0x0004 SIGNAL         payload: [u32 signo]; v1 supports only signo 9 to
//!                       the exec child — any other signo is EXEC_REJECTED
//!                       with reason "signal-not-supported-v1"
//! 0x0005 SHUTDOWN       payload: empty
//! 0x0006 EXEC_STDIN     payload: raw bytes (v1 reserved seam: always
//!                       EXEC_REJECTED "stdin-not-supported-v1")
//! 0x0100..=0x01FF       reserved for the future file API (always
//!                       EXEC_REJECTED "reserved-type"); payload raw bytes
//! ```
//!
//! Message types, guest → host:
//!
//! ```text
//! 0x8001 HEALTH_PONG        [u16 proto_ver = 1][u8 n][n bytes target triple]
//! 0x8002 EXEC_ACCEPTED      empty
//! 0x8003 EXEC_REJECTED      [u16 n][n bytes UTF-8 reason]
//! 0x8004 EXEC_STDOUT_CHUNK  raw bytes, ≤ 64 KiB
//! 0x8005 EXEC_STDERR_CHUNK  raw bytes, ≤ 64 KiB
//! 0x8006 EXEC_FINISHED      [u8 kind][kind payload]:
//!                             0 exited       → [i32 exit_code]
//!                             1 signaled     → [u32 signo]
//!                             2 timeout      (empty)
//!                             3 cancelled    (empty)
//!                             4 spawn-failed → [u16 n][UTF-8 reason]
//! 0x8007 SHUTDOWN_DONE      empty
//! ```
//!
//! Fail-closed rule: bad magic, a version ≠ 1, an unknown type, an oversized
//! `payload_len`, a truncated or malformed payload, a non-empty payload on an
//! empty-payload type, trailing bytes after a parsed payload, or a frame
//! carrying a guest→host type into the agent — each drops the connection
//! (both fds close) WITHOUT guessing and WITHOUT writing anything to the
//! guest console that could corrupt the channel. The agent must not exit or
//! kill the guest because of it. Once the channel is dropped it is dead: no
//! further frame is read, written, or answered on it.
//!
//! Semantic (well-formed) rejections are NOT drops; they are answered with
//! EXEC_REJECTED: "busy", "signal-not-supported-v1", "stdin-not-supported-v1",
//! "reserved-type".

use std::fmt;
use std::io::{Read, Write};

/// Frame magic, bytes 0..4 of every header.
pub const MAGIC: [u8; 4] = *b"LSGP";
/// The only protocol version this codec accepts.
pub const PROTO_VERSION: u16 = 1;
/// Total header length in bytes.
pub const HEADER_LEN: usize = 16;
/// Hard cap on `payload_len`; larger frames drop the channel unread.
pub const MAX_PAYLOAD: usize = 1024 * 1024; // 1 MiB
/// Upper bound of a single stdout/stderr chunk frame.
pub const MAX_CHUNK: usize = 64 * 1024; // 64 KiB

/// Well-known message type tags.
pub mod ty {
    pub const HEALTH_PING: u16 = 0x0001;
    pub const EXEC_START: u16 = 0x0002;
    pub const EXEC_CANCEL: u16 = 0x0003;
    pub const SIGNAL: u16 = 0x0004;
    pub const SHUTDOWN: u16 = 0x0005;
    pub const EXEC_STDIN: u16 = 0x0006;
    pub const HEALTH_PONG: u16 = 0x8001;
    pub const EXEC_ACCEPTED: u16 = 0x8002;
    pub const EXEC_REJECTED: u16 = 0x8003;
    pub const EXEC_STDOUT_CHUNK: u16 = 0x8004;
    pub const EXEC_STDERR_CHUNK: u16 = 0x8005;
    pub const EXEC_FINISHED: u16 = 0x8006;
    pub const SHUTDOWN_DONE: u16 = 0x8007;
}

/// 0x0100..=0x01FF is reserved for the future file API.
pub fn is_reserved_file_api(t: u16) -> bool {
    (0x0100..=0x01ff).contains(&t)
}

/// A decoded frame: header request id plus typed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub request_id: u32,
    pub body: Body,
}

impl Frame {
    pub fn new(request_id: u32, body: Body) -> Self {
        Frame { request_id, body }
    }
}

/// Typed frame payload. Direction matters: the agent accepts only the
/// host→guest variants; guest→host variants exist so the supervisor side and
/// the tests can decode what the agent emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    // host → guest
    HealthPing,
    ExecStart(ExecStart),
    ExecCancel,
    Signal {
        signo: u32,
    },
    Shutdown,
    ExecStdin(Vec<u8>),
    ReservedFileApi(Vec<u8>),
    // guest → host
    HealthPong {
        proto_ver: u16,
        target: String,
    },
    ExecAccepted,
    ExecRejected {
        reason: String,
    },
    StdoutChunk(Vec<u8>),
    StderrChunk(Vec<u8>),
    Finished(Finished),
    ShutdownDone,
}

/// Parsed EXEC_START payload. argv and cwd stay raw bytes (never UTF-8
/// guessed); the unix exec layer turns them into `OsStr` losslessly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecStart {
    /// 0 = no timeout.
    pub timeout_ms: u32,
    pub cwd: Vec<u8>,
    /// Never empty: argv_count = 0 is malformed.
    pub argv: Vec<Vec<u8>>,
}

/// EXEC_FINISHED kind union, tags 0..=4 as documented above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    Exited(i32),
    Signaled(u32),
    Timeout,
    Cancelled,
    SpawnFailed(String),
}

/// Fail-closed decode failures; every one of them drops the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    BadMagic,
    UnsupportedVersion(u16),
    UnknownType(u16),
    /// A syntactically fine frame whose type belongs to the guest→host
    /// direction arrived at the agent.
    WrongDirection(u16),
    OversizedPayload(u32),
    Malformed(&'static str),
    /// EOF after the first byte of a header or inside a payload.
    Truncated,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::BadMagic => write!(f, "bad magic"),
            Violation::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Violation::UnknownType(t) => write!(f, "unknown message type 0x{t:04x}"),
            Violation::WrongDirection(t) => {
                write!(f, "guest-to-host type 0x{t:04x} received by agent")
            }
            Violation::OversizedPayload(n) => write!(f, "payload_len {n} exceeds 1 MiB cap"),
            Violation::Malformed(why) => write!(f, "malformed payload: {why}"),
            Violation::Truncated => write!(f, "connection closed mid-frame"),
        }
    }
}

impl std::error::Error for Violation {}

/// Local encode-time contract violation (never a wire event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    ChunkTooLarge(usize),
    StringTooLong(&'static str, usize),
    PayloadTooLarge(usize),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::ChunkTooLarge(n) => write!(f, "chunk of {n} bytes exceeds 64 KiB"),
            EncodeError::StringTooLong(what, n) => write!(f, "{what} of {n} bytes too long"),
            EncodeError::PayloadTooLarge(n) => write!(f, "payload of {n} bytes exceeds 1 MiB"),
        }
    }
}

impl std::error::Error for EncodeError {}

// ------------------------------------------------------------------- decode

/// Exact-position payload parser: every take is bounds-checked, `finish`
/// rejects trailing bytes. Anything unexpected is malformed → drop.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Violation> {
        if self.remaining() < n {
            return Err(Violation::Malformed("field runs past payload end"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, Violation> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Violation> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, Violation> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, Violation> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn utf8(&mut self, n: usize) -> Result<String, Violation> {
        let raw = self.take(n)?;
        String::from_utf8(raw.to_vec()).map_err(|_| Violation::Malformed("invalid UTF-8"))
    }

    /// The whole payload must be consumed: trailing bytes are an error.
    fn finish(self) -> Result<(), Violation> {
        if self.remaining() > 0 {
            return Err(Violation::Malformed("trailing bytes after parsed fields"));
        }
        Ok(())
    }

    /// Empty-payload types: one unexpected byte is already malformed.
    fn expect_empty(buf: &[u8]) -> Result<(), Violation> {
        if !buf.is_empty() {
            return Err(Violation::Malformed("unexpected payload on empty-payload type"));
        }
        Ok(())
    }
}

fn decode_body(t: u16, payload: &[u8]) -> Result<Body, Violation> {
    match t {
        ty::HEALTH_PING => {
            Cursor::expect_empty(payload)?;
            Ok(Body::HealthPing)
        }
        ty::EXEC_START => {
            let mut c = Cursor::new(payload);
            let timeout_ms = c.u32()?;
            let cwd_len = c.u16()? as usize;
            let cwd = c.take(cwd_len)?.to_vec();
            let argv_count = c.u16()?;
            if argv_count == 0 {
                return Err(Violation::Malformed("argv_count is 0"));
            }
            let mut argv = Vec::with_capacity(argv_count as usize);
            for _ in 0..argv_count {
                let len = c.u32()? as usize;
                argv.push(c.take(len)?.to_vec());
            }
            c.finish()?;
            Ok(Body::ExecStart(ExecStart {
                timeout_ms,
                cwd,
                argv,
            }))
        }
        ty::EXEC_CANCEL => {
            Cursor::expect_empty(payload)?;
            Ok(Body::ExecCancel)
        }
        ty::SIGNAL => {
            let mut c = Cursor::new(payload);
            let signo = c.u32()?;
            c.finish()?;
            Ok(Body::Signal { signo })
        }
        ty::SHUTDOWN => {
            Cursor::expect_empty(payload)?;
            Ok(Body::Shutdown)
        }
        ty::EXEC_STDIN => Ok(Body::ExecStdin(payload.to_vec())),
        t if is_reserved_file_api(t) => Ok(Body::ReservedFileApi(payload.to_vec())),
        ty::HEALTH_PONG => {
            let mut c = Cursor::new(payload);
            let proto_ver = c.u16()?;
            let n = c.u8()? as usize;
            let target = c.utf8(n)?;
            c.finish()?;
            Ok(Body::HealthPong { proto_ver, target })
        }
        ty::EXEC_ACCEPTED => {
            Cursor::expect_empty(payload)?;
            Ok(Body::ExecAccepted)
        }
        ty::EXEC_REJECTED => {
            let mut c = Cursor::new(payload);
            let n = c.u16()? as usize;
            let reason = c.utf8(n)?;
            c.finish()?;
            Ok(Body::ExecRejected { reason })
        }
        ty::EXEC_STDOUT_CHUNK => {
            if payload.len() > MAX_CHUNK {
                return Err(Violation::Malformed("stdout chunk exceeds 64 KiB"));
            }
            Ok(Body::StdoutChunk(payload.to_vec()))
        }
        ty::EXEC_STDERR_CHUNK => {
            if payload.len() > MAX_CHUNK {
                return Err(Violation::Malformed("stderr chunk exceeds 64 KiB"));
            }
            Ok(Body::StderrChunk(payload.to_vec()))
        }
        ty::EXEC_FINISHED => {
            let mut c = Cursor::new(payload);
            let kind = c.u8()?;
            let finished = match kind {
                0 => Finished::Exited(c.i32()?),
                1 => Finished::Signaled(c.u32()?),
                2 => Finished::Timeout,
                3 => Finished::Cancelled,
                4 => {
                    let n = c.u16()? as usize;
                    Finished::SpawnFailed(c.utf8(n)?)
                }
                _ => return Err(Violation::Malformed("unknown finished kind")),
            };
            c.finish()?;
            Ok(Body::Finished(finished))
        }
        ty::SHUTDOWN_DONE => {
            Cursor::expect_empty(payload)?;
            Ok(Body::ShutdownDone)
        }
        t => Err(Violation::UnknownType(t)),
    }
}

/// The tag a body serializes with (also its direction).
pub fn body_type(body: &Body) -> u16 {
    match body {
        Body::HealthPing => ty::HEALTH_PING,
        Body::ExecStart(_) => ty::EXEC_START,
        Body::ExecCancel => ty::EXEC_CANCEL,
        Body::Signal { .. } => ty::SIGNAL,
        Body::Shutdown => ty::SHUTDOWN,
        Body::ExecStdin(_) => ty::EXEC_STDIN,
        Body::ReservedFileApi(_) => 0x0100, // canonical reserved slot
        Body::HealthPong { .. } => ty::HEALTH_PONG,
        Body::ExecAccepted => ty::EXEC_ACCEPTED,
        Body::ExecRejected { .. } => ty::EXEC_REJECTED,
        Body::StdoutChunk(_) => ty::EXEC_STDOUT_CHUNK,
        Body::StderrChunk(_) => ty::EXEC_STDERR_CHUNK,
        Body::Finished(_) => ty::EXEC_FINISHED,
        Body::ShutdownDone => ty::SHUTDOWN_DONE,
    }
}

/// True for the tags a host sends to the guest agent.
pub fn is_host_to_guest(t: u16) -> bool {
    matches!(
        t,
        ty::HEALTH_PING
            | ty::EXEC_START
            | ty::EXEC_CANCEL
            | ty::SIGNAL
            | ty::SHUTDOWN
            | ty::EXEC_STDIN
    ) || is_reserved_file_api(t)
}

// ------------------------------------------------------------------- encode

fn encode_body(body: &Body, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    match body {
        Body::HealthPing
        | Body::ExecCancel
        | Body::Shutdown
        | Body::ExecAccepted
        | Body::ShutdownDone => {}
        Body::ExecStart(spec) => {
            if spec.cwd.len() > u16::MAX as usize {
                return Err(EncodeError::StringTooLong("cwd", spec.cwd.len()));
            }
            if spec.argv.len() > u16::MAX as usize {
                return Err(EncodeError::StringTooLong("argv", spec.argv.len()));
            }
            out.extend_from_slice(&spec.timeout_ms.to_be_bytes());
            out.extend_from_slice(&(spec.cwd.len() as u16).to_be_bytes());
            out.extend_from_slice(&spec.cwd);
            out.extend_from_slice(&(spec.argv.len() as u16).to_be_bytes());
            for arg in &spec.argv {
                out.extend_from_slice(&(arg.len() as u32).to_be_bytes());
                out.extend_from_slice(arg);
            }
        }
        Body::Signal { signo } => out.extend_from_slice(&signo.to_be_bytes()),
        Body::ExecStdin(bytes) | Body::ReservedFileApi(bytes) => out.extend_from_slice(bytes),
        Body::HealthPong { proto_ver, target } => {
            if target.len() > u8::MAX as usize {
                return Err(EncodeError::StringTooLong("target", target.len()));
            }
            out.extend_from_slice(&proto_ver.to_be_bytes());
            out.push(target.len() as u8);
            out.extend_from_slice(target.as_bytes());
        }
        Body::ExecRejected { reason } => {
            if reason.len() > u16::MAX as usize {
                return Err(EncodeError::StringTooLong("reason", reason.len()));
            }
            out.extend_from_slice(&(reason.len() as u16).to_be_bytes());
            out.extend_from_slice(reason.as_bytes());
        }
        Body::StdoutChunk(bytes) | Body::StderrChunk(bytes) => {
            if bytes.len() > MAX_CHUNK {
                return Err(EncodeError::ChunkTooLarge(bytes.len()));
            }
            out.extend_from_slice(bytes);
        }
        Body::Finished(finished) => match finished {
            Finished::Exited(code) => {
                out.push(0);
                out.extend_from_slice(&code.to_be_bytes());
            }
            Finished::Signaled(signo) => {
                out.push(1);
                out.extend_from_slice(&signo.to_be_bytes());
            }
            Finished::Timeout => out.push(2),
            Finished::Cancelled => out.push(3),
            Finished::SpawnFailed(detail) => {
                if detail.len() > u16::MAX as usize {
                    return Err(EncodeError::StringTooLong("reason", detail.len()));
                }
                out.push(4);
                out.extend_from_slice(&(detail.len() as u16).to_be_bytes());
                out.extend_from_slice(detail.as_bytes());
            }
        },
    }
    Ok(())
}

/// Serialize a frame to header + payload, enforcing every length bound.
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, EncodeError> {
    let mut payload = Vec::new();
    encode_body(&frame.body, &mut payload)?;
    if payload.len() > MAX_PAYLOAD {
        return Err(EncodeError::PayloadTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&PROTO_VERSION.to_be_bytes());
    out.extend_from_slice(&body_type(&frame.body).to_be_bytes());
    out.extend_from_slice(&frame.request_id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Frame-read outcome: either a full frame, a clean EOF at a frame boundary,
/// or a fail-closed violation that drops the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameRead {
    Frame(Frame),
    Eof,
    Violation(Violation),
}

/// Read exactly one frame; tolerates arbitrarily short reads. `Err` is a
/// real I/O error only — every protocol problem is reported as
/// [`FrameRead::Violation`] so the caller can drop the channel without ever
/// confusing a wire error with an OS error.
pub fn read_frame(r: &mut impl Read) -> std::io::Result<FrameRead> {
    let mut hdr = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        match r.read(&mut hdr[got..]) {
            Ok(0) => {
                return Ok(if got == 0 {
                    FrameRead::Eof
                } else {
                    FrameRead::Violation(Violation::Truncated)
                });
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    if hdr[0..4] != MAGIC {
        return Ok(FrameRead::Violation(Violation::BadMagic));
    }
    let version = u16::from_be_bytes(hdr[4..6].try_into().unwrap());
    if version != PROTO_VERSION {
        return Ok(FrameRead::Violation(Violation::UnsupportedVersion(version)));
    }
    let msg_type = u16::from_be_bytes(hdr[6..8].try_into().unwrap());
    let request_id = u32::from_be_bytes(hdr[8..12].try_into().unwrap());
    let payload_len = u32::from_be_bytes(hdr[12..16].try_into().unwrap());
    if payload_len as usize > MAX_PAYLOAD {
        // Close without reading: the payload never enters memory.
        return Ok(FrameRead::Violation(Violation::OversizedPayload(payload_len)));
    }
    let mut payload = vec![0u8; payload_len as usize];
    let mut got = 0;
    while got < payload.len() {
        match r.read(&mut payload[got..]) {
            Ok(0) => return Ok(FrameRead::Violation(Violation::Truncated)),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    match decode_body(msg_type, &payload) {
        Ok(body) => Ok(FrameRead::Frame(Frame { request_id, body })),
        Err(v) => Ok(FrameRead::Violation(v)),
    }
}

/// Encode and write one frame; `write_all` absorbs short writes.
pub fn write_frame(w: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    let bytes = encode_frame(frame).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    w.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn roundtrip(frame: &Frame) -> Frame {
        let bytes = encode_frame(frame).unwrap();
        let mut cur = std::io::Cursor::new(bytes);
        match read_frame(&mut cur).unwrap() {
            FrameRead::Frame(f) => f,
            other => panic!("expected full frame, got {other:?}"),
        }
    }

    #[test]
    fn roundtrips_every_message_type() {
        let cases = vec![
            Frame::new(1, Body::HealthPing),
            Frame::new(
                2,
                Body::ExecStart(ExecStart {
                    timeout_ms: 1500,
                    cwd: b"/work dir".to_vec(),
                    argv: vec![b"/bin/sh".to_vec(), b"-c".to_vec(), b"echo hi".to_vec()],
                }),
            ),
            Frame::new(3, Body::ExecCancel),
            Frame::new(4, Body::Signal { signo: 9 }),
            Frame::new(5, Body::Shutdown),
            Frame::new(6, Body::ExecStdin(b"stdin bytes".to_vec())),
            Frame::new(7, Body::ReservedFileApi(vec![1, 2, 3])),
            Frame::new(
                8,
                Body::HealthPong {
                    proto_ver: 1,
                    target: "x86_64-unknown-linux-musl".to_string(),
                },
            ),
            Frame::new(9, Body::ExecAccepted),
            Frame::new(10, Body::ExecRejected { reason: "busy".into() }),
            Frame::new(11, Body::StdoutChunk(vec![b'o'; 64 * 1024])),
            Frame::new(12, Body::StderrChunk(vec![7; 1])),
            Frame::new(13, Body::Finished(Finished::Exited(-1))),
            Frame::new(14, Body::Finished(Finished::Signaled(9))),
            Frame::new(15, Body::Finished(Finished::Timeout)),
            Frame::new(16, Body::Finished(Finished::Cancelled)),
            Frame::new(17, Body::Finished(Finished::SpawnFailed("no such file".into()))),
            Frame::new(18, Body::ShutdownDone),
        ];
        for frame in cases {
            assert_eq!(roundtrip(&frame), frame, "lost fidelity: {frame:?}");
        }
    }

    #[test]
    fn roundtrips_exec_start_with_binary_args() {
        let frame = Frame::new(
            0xdead_beef,
            Body::ExecStart(ExecStart {
                timeout_ms: 0,
                cwd: vec![],
                argv: vec![
                    b"/bin/echo".to_vec(),
                    vec![0xff, 0x00, 0xfe], // not UTF-8, must stay raw
                    vec![],                 // empty arg is legal
                ],
            }),
        );
        assert_eq!(roundtrip(&frame), frame);
    }

    /// A reader that hands out at most `step` bytes per read call, so the
    /// decoder must reassemble frames byte-fragment by byte-fragment.
    struct Trickle {
        buf: VecDeque<u8>,
        step: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = out.len().min(self.step).min(self.buf.len());
            for slot in out.iter_mut().take(n) {
                *slot = self.buf.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    #[test]
    fn decodes_frames_arriving_in_arbitrary_fragments() {
        let f1 = Frame::new(
            1,
            Body::ExecStart(ExecStart {
                timeout_ms: 5,
                cwd: b"/".to_vec(),
                argv: vec![b"/bin/true".to_vec()],
            }),
        );
        let f2 = Frame::new(2, Body::HealthPing);
        let mut bytes = encode_frame(&f1).unwrap();
        bytes.extend(encode_frame(&f2).unwrap());
        for step in [1, 3, 7, HEADER_LEN - 1] {
            let mut t = Trickle {
                buf: bytes.clone().into(),
                step,
            };
            assert_eq!(read_frame(&mut t).unwrap(), FrameRead::Frame(f1.clone()));
            assert_eq!(read_frame(&mut t).unwrap(), FrameRead::Frame(f2.clone()));
            assert_eq!(read_frame(&mut t).unwrap(), FrameRead::Eof);
        }
    }

    /// A writer that never writes more than `cap` bytes per call.
    struct ShortWriter {
        buf: Vec<u8>,
        cap: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let n = bytes.len().min(self.cap);
            self.buf.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn encoder_tolerates_short_writes() {
        let frame = Frame::new(9, Body::StdoutChunk(vec![b'x'; 1000]));
        for cap in [1, 5, 13] {
            let mut w = ShortWriter {
                buf: Vec::new(),
                cap,
            };
            write_frame(&mut w, &frame).unwrap();
            assert_eq!(w.buf, encode_frame(&frame).unwrap());
        }
    }

    fn violation_of(bytes: &[u8]) -> Violation {
        let mut cur = std::io::Cursor::new(bytes.to_vec());
        match read_frame(&mut cur).unwrap() {
            FrameRead::Violation(v) => v,
            other => panic!("expected violation, got {other:?}"),
        }
    }

    fn framed(msg_type: u16, request_id: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&msg_type.to_be_bytes());
        b.extend_from_slice(&request_id.to_be_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = framed(ty::HEALTH_PING, 1, &[]);
        bytes[0] = b'X';
        assert_eq!(violation_of(&bytes), Violation::BadMagic);
    }

    #[test]
    fn rejects_wrong_version() {
        let mut bytes = framed(ty::HEALTH_PING, 1, &[]);
        bytes[5] = 2; // version 2
        assert_eq!(violation_of(&bytes), Violation::UnsupportedVersion(2));
    }

    #[test]
    fn rejects_unknown_type() {
        assert_eq!(
            violation_of(&framed(0x1234, 1, &[])),
            Violation::UnknownType(0x1234)
        );
    }

    #[test]
    fn rejects_payload_larger_than_1_mib_without_reading_it() {
        let mut hdr = Vec::new();
        hdr.extend_from_slice(&MAGIC);
        hdr.extend_from_slice(&1u16.to_be_bytes());
        hdr.extend_from_slice(&ty::HEALTH_PING.to_be_bytes());
        hdr.extend_from_slice(&1u32.to_be_bytes());
        hdr.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_be_bytes());
        // No payload present at all; the decoder must fail from the header.
        assert_eq!(
            violation_of(&hdr),
            Violation::OversizedPayload((MAX_PAYLOAD + 1) as u32)
        );
    }

    #[test]
    fn rejects_empty_exec_start_payload() {
        assert_eq!(
            violation_of(&framed(ty::EXEC_START, 1, &[])),
            Violation::Malformed("field runs past payload end")
        );
    }

    #[test]
    fn rejects_zero_argv_count() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_be_bytes()); // timeout
        payload.extend_from_slice(&0u16.to_be_bytes()); // cwd_len
        payload.extend_from_slice(&0u16.to_be_bytes()); // argv_count = 0
        assert_eq!(
            violation_of(&framed(ty::EXEC_START, 1, &payload)),
            Violation::Malformed("argv_count is 0")
        );
    }

    #[test]
    fn rejects_arg_length_running_past_end() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes()); // one arg
        payload.extend_from_slice(&9u32.to_be_bytes()); // claims 9 bytes
        payload.extend_from_slice(b"ab"); // 2 present
        assert_eq!(
            violation_of(&framed(ty::EXEC_START, 1, &payload)),
            Violation::Malformed("field runs past payload end")
        );
    }

    #[test]
    fn rejects_exec_start_trailing_byte() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&4u32.to_be_bytes());
        payload.extend_from_slice(b"/bin");
        payload.push(0); // one byte too many
        assert_eq!(
            violation_of(&framed(ty::EXEC_START, 1, &payload)),
            Violation::Malformed("trailing bytes after parsed fields")
        );
    }

    #[test]
    fn rejects_non_empty_payload_on_empty_payload_types() {
        for t in [ty::HEALTH_PING, ty::EXEC_CANCEL, ty::SHUTDOWN] {
            assert_eq!(
                violation_of(&framed(t, 1, &[0])),
                Violation::Malformed("unexpected payload on empty-payload type")
            );
        }
    }

    #[test]
    fn rejects_truncated_header_and_payload() {
        let full = framed(ty::HEALTH_PING, 1, &[]);
        assert_eq!(violation_of(&full[..8]), Violation::Truncated);

        let chunk = framed(ty::EXEC_STDOUT_CHUNK, 1, b"abcdef");
        let cut = &chunk[..HEADER_LEN + 3];
        assert_eq!(violation_of(cut), Violation::Truncated);
    }

    #[test]
    fn rejects_oversized_chunk_on_decode() {
        let payload = vec![0u8; MAX_CHUNK + 1];
        assert_eq!(
            violation_of(&framed(ty::EXEC_STDOUT_CHUNK, 1, &payload)),
            Violation::Malformed("stdout chunk exceeds 64 KiB")
        );
    }

    #[test]
    fn rejects_unknown_finished_kind() {
        assert_eq!(
            violation_of(&framed(ty::EXEC_FINISHED, 1, &[9])),
            Violation::Malformed("unknown finished kind")
        );
    }

    #[test]
    fn rejects_invalid_utf8_in_rejected_reason() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.push(0xff);
        assert_eq!(
            violation_of(&framed(ty::EXEC_REJECTED, 1, &payload)),
            Violation::Malformed("invalid UTF-8")
        );
    }

    #[test]
    fn encode_refuses_oversized_chunk_and_string() {
        assert_eq!(
            encode_frame(&Frame::new(1, Body::StdoutChunk(vec![0; MAX_CHUNK + 1]))),
            Err(EncodeError::ChunkTooLarge(MAX_CHUNK + 1))
        );
        assert_eq!(
            encode_frame(&Frame::new(
                1,
                Body::HealthPong {
                    proto_ver: 1,
                    target: "x".repeat(300)
                }
            )),
            Err(EncodeError::StringTooLong("target", 300))
        );
    }
}
