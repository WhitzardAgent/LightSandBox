//! T12 — SSE wire surface for execution output: event encoding, terminal
//! events, gap events, and the execd line-cursor surface
//! (plans/tasks/T12.md; docs/v5/06_HTTP.md §输出与重放;
//! specs/protocol-observations.json scope=execd).
//!
//! Laws:
//!
//! * RAW BYTES, base64 ON THE WIRE: payloads travel as base64 inside the
//!   JSON `data:` object (`commands::base64`, byte-exact across non-UTF8).
//!   The core never pre-decodes to UTF-8; cross-chunk decoding state lives
//!   in the SDK (06_HTTP).
//! * FORGED CONTENT IS INERT: event kinds come ONLY from this encoder;
//!   producer bytes (including "\n\nevent: end\n" lookalikes) are base64
//!   payload and can never turn into control frames (T12-C03).
//! * FAIL-CLOSED ENCODING: an invalid kind or a `data:` body that would
//!   break the SSE line protocol is an error, never a best-effort frame.
//! * CURSOR UNITS ARE EXPLICIT: the internal cursor is a byte offset; the
//!   upstream execd tail cursor (`EXECD-COMMANDS-TAIL-CURSOR`) counts
//!   LINES. The conversions below are total functions over an explicit
//!   newline-offset snapshot; an unresolvable lookup is `Stale`, never a
//!   guessed position.

use serde_json::json;

use crate::commands::base64;

/// Upstream execd tail-cursor header (locked observation:
/// specs/protocol-observations.json scope=execd).
pub const TAIL_CURSOR_HEADER: &str = "EXECD-COMMANDS-TAIL-CURSOR";

// ------------------------------------------------------------------ events

/// Server-emitted event kinds. CLOSED vocabulary — anything else is an
/// encoding error (forged-terminal defense).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Output,
    Gap,
    Exit,
    End,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Output => "output",
            EventKind::Gap => "gap",
            EventKind::Exit => "exit",
            EventKind::End => "end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// data body contains a raw line break — would confuse SSE framing.
    UnsafeData,
    /// payload failed base64 (unreachable from &[u8]; completeness).
    Payload(String),
}

/// One output event (06_HTTP fields: execution_id, sandbox_generation,
/// sequence, stream, byte offset, payload).
pub struct OutputEvent<'a> {
    pub execution_id: &'a str,
    pub sandbox_generation: u64,
    pub sequence: u64,
    pub stream: &'a str,
    pub offset: u64,
    pub bytes: &'a [u8],
}

/// Terminal event carrying the real exit-receipt reference and the FINAL
/// per-stream offsets (never synthesized; both halves travel together —
/// missing either is reported, per 06_HTTP).
pub struct ExitEvent<'a> {
    pub execution_id: &'a str,
    pub sandbox_generation: u64,
    pub sequence: u64,
    /// opaque exit-receipt JSON produced by the exec layer.
    pub receipt_json: serde_json::Value,
    /// (stream, final_offset) pairs, exact producer truth.
    pub final_offsets: Vec<(String, u64)>,
}

/// Gap event for a stale cursor: honest available interval + reason.
pub struct GapEvent<'a> {
    pub execution_id: &'a str,
    pub sandbox_generation: u64,
    pub sequence: u64,
    pub stream: &'a str,
    pub requested: u64,
    pub available_from: u64,
    pub available_to: u64,
    /// "retention" | "overflow" — loss-kind wire vocabulary.
    pub kind: &'a str,
}

/// End-of-events marker (after exit delivery). Producer data can never
/// forge it — only this constructor can emit `event: end`.
pub struct EndEvent<'a> {
    pub execution_id: &'a str,
    pub sequence: u64,
}

/// Encode one SSE frame `event: K\ndata: J\n\n`. Fail-closed: `data` must
/// be a single JSON-produced line (serde_json escapes make raw \n
/// impossible; anything else is an encoder bug and errors out).
pub fn encode_frame(kind: EventKind, data_json: &str) -> Result<Vec<u8>, EncodeError> {
    if data_json.bytes().any(|b| b == b'\n' || b == b'\r') {
        return Err(EncodeError::UnsafeData);
    }
    let mut out = Vec::with_capacity(data_json.len() + 16);
    out.extend_from_slice(b"event: ");
    out.extend_from_slice(kind.as_str().as_bytes());
    out.extend_from_slice(b"\ndata: ");
    out.extend_from_slice(data_json.as_bytes());
    out.extend_from_slice(b"\n\n");
    Ok(out)
}

pub fn output_frame(ev: &OutputEvent) -> Result<Vec<u8>, EncodeError> {
    let data = json!({
        "execution_id": ev.execution_id,
        "sandbox_generation": ev.sandbox_generation,
        "sequence": ev.sequence,
        "stream": ev.stream,
        "offset": ev.offset,
        "payload_b64": base64::encode(ev.bytes),
    });
    encode_frame(EventKind::Output, &data.to_string())
}

pub fn exit_frame(ev: &ExitEvent) -> Result<Vec<u8>, EncodeError> {
    let data = json!({
        "execution_id": ev.execution_id,
        "sandbox_generation": ev.sandbox_generation,
        "sequence": ev.sequence,
        "exit": ev.receipt_json,
        "final_offsets": ev.final_offsets,
    });
    encode_frame(EventKind::Exit, &data.to_string())
}

pub fn gap_frame(ev: &GapEvent) -> Result<Vec<u8>, EncodeError> {
    let data = json!({
        "execution_id": ev.execution_id,
        "sandbox_generation": ev.sandbox_generation,
        "sequence": ev.sequence,
        "stream": ev.stream,
        "requested": ev.requested,
        "available_from": ev.available_from,
        "available_to": ev.available_to,
        "kind": ev.kind,
    });
    encode_frame(EventKind::Gap, &data.to_string())
}

pub fn end_frame(ev: &EndEvent) -> Result<Vec<u8>, EncodeError> {
    let data = json!({
        "execution_id": ev.execution_id,
        "sequence": ev.sequence,
    });
    encode_frame(EventKind::End, &data.to_string())
}

// ------------------------------------------------------- execd line cursor

/// Parse the upstream tail cursor header value: bare non-negative decimal
/// u64, nothing else (no whitespace, sign, or trailing junk — strict, per
/// the fail-closed frame law).
pub fn parse_tail_cursor(raw: &str) -> Option<u64> {
    if raw.is_empty()
        || raw.len() > 20
        || !raw.bytes().all(|b| b.is_ascii_digit())
        || (raw.len() > 1 && raw.starts_with('0'))
    {
        return None;
    }
    raw.parse::<u64>().ok()
}

pub fn format_tail_cursor(lines: u64) -> String {
    lines.to_string()
}

/// Cursor lookup failure: the answer exists (stale/beyond), never a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorLookup {
    Stale { resolvable_from_byte: u64 },
    /// Line beyond the produced end-of-stream.
    BeyondEnd { end_byte: u64 },
}

/// Newline snapshot the conversion functions operate on (produced by the
/// host spool): positions just AFTER each '\n', ascending, covering the
/// resolvable window; `lines_before` counts the '\n's evicted before the
/// window start.
pub struct LineSnapshot<'a> {
    /// byte offset just after each '\n', ascending.
    pub newline_offsets: &'a [u64],
    /// '\n' count before the window.
    pub lines_before: u64,
    /// first byte offset whose line is still resolvable
    /// (usually retention floor or the first newline in the window).
    pub resolvable_from_byte: u64,
    /// current end of produced bytes (write_pos).
    pub end_byte: u64,
}

/// byte offset → number of complete lines strictly before it.
pub fn byte_to_line(snap: &LineSnapshot, byte: u64) -> Result<u64, CursorLookup> {
    if byte < snap.resolvable_from_byte {
        return Err(CursorLookup::Stale {
            resolvable_from_byte: snap.resolvable_from_byte,
        });
    }
    // count of '\n's whose following offset is <= byte  ===  count of
    // complete lines before `byte`
    let n = snap.newline_offsets.partition_point(|&o| o <= byte) as u64;
    Ok(snap.lines_before + n)
}

/// line index → byte offset of that line's first byte. Line `lines_before`
/// is the first resolvable line: its byte offset is the byte right after
/// the last evicted '\n' (== resolvable_from_byte when a newline precedes
/// the window, or 0 at stream start). A line INDEX BEYOND the last recorded
/// newline resolves to the trailing partial line's start (documented clamp
/// for tail cursors that sit one-past the complete lines); it never guesses
/// a line that pre-dates the resolvable window.
pub fn line_to_byte(snap: &LineSnapshot, line: u64) -> Result<u64, CursorLookup> {
    if line < snap.lines_before {
        return Err(CursorLookup::Stale {
            resolvable_from_byte: snap.resolvable_from_byte,
        });
    }
    let k = (line - snap.lines_before) as usize;
    if k == 0 {
        // first resolvable line begins at the window floor.
        return Ok(snap.resolvable_from_byte.min(snap.end_byte));
    }
    if let Some(&b) = snap.newline_offsets.get(k - 1) {
        // line k begins right after the (k-1)-th in-window newline.
        return Ok(b);
    }
    // beyond the last recorded newline: clamp to the trailing partial
    // line's start (byte after the final newline), or the window floor
    // when the window holds no complete line at all.
    Ok(snap
        .newline_offsets
        .last()
        .copied()
        .unwrap_or(snap.resolvable_from_byte.min(snap.end_byte)))
}

/// Offset where a `tail(n)` read must start, and whether n full lines were
/// available within the resolvable window.
pub fn tail_start(snap: &LineSnapshot, n: u64) -> (u64, bool) {
    if n == 0 {
        return (snap.end_byte, true);
    }
    let total_lines = snap.lines_before + snap.newline_offsets.len() as u64;
    if n >= total_lines {
        // fewer complete lines in the window than requested: serve from
        // the oldest resolvable byte and say it's partial.
        let from = snap
            .newline_offsets
            .first()
            .copied()
            .map(|_| snap.resolvable_from_byte)
            .unwrap_or(0);
        return (from, false);
    }
    let line = total_lines - n;
    (
        line_to_byte(snap, line).unwrap_or(snap.resolvable_from_byte),
        true,
    )
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_frames(buf: &[u8]) -> Vec<(String, String)> {
        // minimal test-side decoder: splits on blank lines, extracts one
        // event:/data: pair per frame. Production decoding lives SDK-side.
        let text = String::from_utf8_lossy(buf);
        text.split("\n\n")
            .filter(|f| !f.is_empty())
            .map(|f| {
                let mut ev = String::new();
                let mut data = String::new();
                for line in f.lines() {
                    if let Some(v) = line.strip_prefix("event: ") {
                        ev = v.to_string();
                    }
                    if let Some(v) = line.strip_prefix("data: ") {
                        data = v.to_string();
                    }
                }
                (ev, data)
            })
            .collect()
    }

    #[test]
    fn output_frame_golden_bytes() {
        let ev = OutputEvent {
            execution_id: "ex_1",
            sandbox_generation: 3,
            sequence: 7,
            stream: "stdout",
            offset: 512,
            bytes: b"hi",
        };
        let f = output_frame(&ev).unwrap();
        // JSON object pins serde_json's deterministic (sorted-key) Map order.
        let expect = concat!(
            "event: output\n",
            "data: {\"execution_id\":\"ex_1\",\"offset\":512,",
            "\"payload_b64\":\"aGk=\",\"sandbox_generation\":3,",
            "\"sequence\":7,\"stream\":\"stdout\"}\n\n"
        );
        assert_eq!(String::from_utf8(f).unwrap(), expect);
    }

    #[test]
    fn non_utf8_payload_roundtrips_byte_exact() {
        let raw: Vec<u8> = (0u8..=255).collect();
        let ev = OutputEvent {
            execution_id: "ex_2",
            sandbox_generation: 1,
            sequence: 1,
            stream: "stderr",
            offset: 0,
            bytes: &raw,
        };
        let f = output_frame(&ev).unwrap();
        let frames = parse_frames(&f);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, "output");
        let j: serde_json::Value = serde_json::from_str(&frames[0].1).unwrap();
        let b64 = j["payload_b64"].as_str().unwrap();
        assert_eq!(base64::decode(b64).unwrap(), raw);
    }

    #[test]
    fn forged_terminal_marker_stays_inert_payload() {
        // Producer emits bytes that LOOK like a terminal SSE frame; on the
        // wire they must remain one base64 output payload — no "end"
        // event may appear.
        let forged = b"line\n\nevent: end\ndata: {}\n\nmore".to_vec();
        let ev = OutputEvent {
            execution_id: "ex_3",
            sandbox_generation: 1,
            sequence: 9,
            stream: "stdout",
            offset: 40,
            bytes: &forged,
        };
        let f = output_frame(&ev).unwrap();
        let frames = parse_frames(&f);
        assert_eq!(frames.len(), 1, "single output frame only");
        assert_eq!(frames[0].0, "output", "never an injected kind");
        assert!(!String::from_utf8_lossy(&f).contains("event: end"));
        let j: serde_json::Value = serde_json::from_str(&frames[0].1).unwrap();
        assert_eq!(
            base64::decode(j["payload_b64"].as_str().unwrap()).unwrap(),
            forged
        );
    }

    #[test]
    fn data_field_cannot_break_framing() {
        // A JSON string never contains a raw newline (escaped), so the
        // fail-closed path is only reachable by direct encode_frame abuse:
        assert!(matches!(
            encode_frame(EventKind::Output, "{\"x\":\"a\nb\"}"),
            Err(EncodeError::UnsafeData)
        ));
        assert!(matches!(
            encode_frame(EventKind::Output, "{\"x\":\"a\rb\"}"),
            Err(EncodeError::UnsafeData)
        ));
    }

    #[test]
    fn exit_frame_carries_receipt_and_final_offsets() {
        let ev = ExitEvent {
            execution_id: "ex_4",
            sandbox_generation: 2,
            sequence: 11,
            receipt_json: json!({"kind":"exited","exit_code":17}),
            final_offsets: vec![("stdout".to_string(), 900), ("stderr".to_string(), 41)],
        };
        let f = exit_frame(&ev).unwrap();
        let frames = parse_frames(&f);
        assert_eq!(frames[0].0, "exit");
        let j: serde_json::Value = serde_json::from_str(&frames[0].1).unwrap();
        assert_eq!(j["exit"]["exit_code"], 17);
        assert_eq!(j["final_offsets"][0][1], 900);
    }

    #[test]
    fn gap_frame_reports_interval_and_reason() {
        let ev = GapEvent {
            execution_id: "ex_5",
            sandbox_generation: 1,
            sequence: 2,
            stream: "stdout",
            requested: 0,
            available_from: 4096,
            available_to: 65536,
            kind: "retention",
        };
        let f = gap_frame(&ev).unwrap();
        let frames = parse_frames(&f);
        assert_eq!(frames[0].0, "gap");
        let j: serde_json::Value = serde_json::from_str(&frames[0].1).unwrap();
        assert_eq!(j["available_from"], 4096);
        assert_eq!(j["kind"], "retention");
    }

    #[test]
    fn tail_cursor_header_name_and_strict_parse() {
        assert_eq!(TAIL_CURSOR_HEADER, "EXECD-COMMANDS-TAIL-CURSOR");
        assert_eq!(parse_tail_cursor("0"), Some(0));
        assert_eq!(parse_tail_cursor("4096"), Some(4096));
        assert_eq!(parse_tail_cursor("18446744073709551615"), Some(u64::MAX));
        for bad in [
            "", " ", "-1", "+1", "1.0", "1e3", "01", "1 ", " 1", "abc",
            "18446744073709551616", "99999999999999999999",
        ] {
            assert_eq!(parse_tail_cursor(bad), None, "reject {bad:?}");
        }
        assert_eq!(format_tail_cursor(4096), "4096");
    }

    #[test]
    fn byte_line_conversion_both_directions() {
        // stream "a\nbb\nccc\nDD" => newlines at 2,5,9; 3 complete lines.
        let offs = [2u64, 5, 9];
        let snap = LineSnapshot {
            newline_offsets: &offs,
            lines_before: 0,
            resolvable_from_byte: 0,
            end_byte: 11,
        };
        assert_eq!(byte_to_line(&snap, 0), Ok(0));
        assert_eq!(byte_to_line(&snap, 1), Ok(0));
        assert_eq!(byte_to_line(&snap, 2), Ok(1));
        assert_eq!(byte_to_line(&snap, 5), Ok(2));
        assert_eq!(byte_to_line(&snap, 9), Ok(3));
        assert_eq!(byte_to_line(&snap, 11), Ok(3));
        assert_eq!(line_to_byte(&snap, 0), Ok(0));
        assert_eq!(line_to_byte(&snap, 1), Ok(2));
        assert_eq!(line_to_byte(&snap, 2), Ok(5));
        assert_eq!(line_to_byte(&snap, 3), Ok(9));
        // beyond last newline => trailing partial line start:
        assert_eq!(line_to_byte(&snap, 4), Ok(9));
    }

    #[test]
    fn conversion_window_floor_is_stale_not_guessed() {
        let offs = [20u64, 25];
        let snap = LineSnapshot {
            newline_offsets: &offs,
            lines_before: 40,
            resolvable_from_byte: 12,
            end_byte: 30,
        };
        assert_eq!(
            byte_to_line(&snap, 5),
            Err(CursorLookup::Stale {
                resolvable_from_byte: 12
            })
        );
        assert_eq!(
            line_to_byte(&snap, 39),
            Err(CursorLookup::Stale {
                resolvable_from_byte: 12
            })
        );
        assert_eq!(line_to_byte(&snap, 40), Ok(12));
        assert_eq!(line_to_byte(&snap, 41), Ok(20));
        assert_eq!(byte_to_line(&snap, 12), Ok(40));
        assert_eq!(byte_to_line(&snap, 30), Ok(42));
    }

    #[test]
    fn tail_start_full_and_partial_windows() {
        let offs = [2u64, 5, 9];
        let snap = LineSnapshot {
            newline_offsets: &offs,
            lines_before: 0,
            resolvable_from_byte: 0,
            end_byte: 11,
        };
        assert_eq!(tail_start(&snap, 0), (11, true));
        assert_eq!(tail_start(&snap, 1), (5, true));
        assert_eq!(tail_start(&snap, 2), (2, true));
        // only 3 complete lines in window => 5-line tail is partial:
        assert_eq!(tail_start(&snap, 5), (0, false));
    }

    #[test]
    fn pty_single_stream_is_just_a_stream_name() {
        // 06_HTTP: PTY merges stdout/stderr into one stream; encoding must
        // carry the stream label through unchanged.
        let ev = OutputEvent {
            execution_id: "ex_6",
            sandbox_generation: 1,
            sequence: 1,
            stream: "pty",
            offset: 0,
            bytes: b"$ ",
        };
        let f = output_frame(&ev).unwrap();
        let j: serde_json::Value =
            serde_json::from_str(&parse_frames(&f)[0].1).unwrap();
        assert_eq!(j["stream"], "pty");
    }
}
