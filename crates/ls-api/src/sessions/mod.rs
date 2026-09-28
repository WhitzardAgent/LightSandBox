//! T13 — session wire vocabulary (persistent shell): create_session /
//! run_in_session shapes, session refs, and the CONTROL-vs-PAYLOAD frame
//! split (plans/tasks/T13.md; docs/v5/03_DOMAIN.md §资源与生命周期 Session
//! row; contracts/v1/surface.json reserves the `sessions` HTTP group and the
//! `session open/attach/resize/close` CLI as planned).
//!
//! Laws pinned here (card review focus + domain contract):
//!
//! * THE CONTROL CHANNEL IS NOT USER STDOUT: "command finished" and the exit
//!   code travel ONLY as typed [`ControlFrame`] values on the control
//!   channel. [`EncodedChunk`] is payload: opaque base64 bytes plus a stream
//!   label and offset. No layer of this vocabulary ever derives control facts
//!   from payload bytes, so a command that prints every known delimiter —
//!   including byte-identical serializations of real control frames — is
//!   inert (T13-C02). The two types are deliberately NOT inter-convertible:
//!   the transport's channel tag, never the content, decides which parser a
//!   frame meets.
//! * FAIL-CLOSED DECODE: unknown fields, unknown enum kinds, wrong scalar
//!   types (bool for seq), and explicit JSON null for an optional field
//!   (null != missing, the frozen execspec discipline) are all
//!   INVALID_ARGUMENT naming the offending field — never a best-effort parse.
//! * BOUNDED INPUT: scripts are capped at [`MAX_SCRIPT_BYTES`], chunks at
//!   [`MAX_CHUNK_BYTES`] (the guest protocol's 64 KiB chunk discipline).
//! * IDs ARE OPAQUE AND SERVER-SIDE: [`SessionId`] mirrors the ls-core
//!   domain id rules (1..=128 of `[A-Za-z0-9._-]`). Lifting it into
//!   `ls_core::domain::ids` is a designated-contract-owner step; the
//!   validation mirror here is byte-identical by pinned test.
//! * PRODUCT VOCABULARY, NOT A FROZEN CONTRACT: `kind` is a closed enum
//!   (`shell` only). 03_DOMAIN lists PTY sessions as a Session subtype; the
//!   PTY session wire surface belongs to the PTY task chain (T14 hosts its
//!   core in ls-host) and is deliberately NOT guessed today — `pty` is a
//!   decode rejection, not a synonym.

use crate::commands::base64;
use crate::commands::ExitWire;
use ls_core::domain::error::{DomainError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// Upper bound of one run_in_session script in bytes (64 KiB).
pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// Upper bound of one EncodedChunk payload in bytes (64 KiB, mirroring the
/// guest protocol MAX_CHUNK).
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// Domain id length cap (mirrors ls_core::domain::ids::MAX_ID_LEN).
pub const MAX_ID_LEN: usize = 128;

// ----------------------------------------------------------------------- id

/// Opaque server-generated session identity. Validation mirrors
/// `ls_core::domain::ids` exactly (1..=128 chars of `[A-Za-z0-9._-]`);
/// anything else is INVALID_ARGUMENT at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, DomainError> {
        let raw = raw.as_ref();
        if raw.is_empty() {
            return Err(DomainError::invalid("session_id must not be empty"));
        }
        if raw.len() > MAX_ID_LEN {
            return Err(DomainError::invalid(format!(
                "session_id exceeds {MAX_ID_LEN} chars"
            )));
        }
        for ch in raw.chars() {
            let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
            if !ok {
                return Err(DomainError::invalid(format!(
                    "session_id contains illegal character {ch:?}"
                )));
            }
        }
        Ok(SessionId(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// ------------------------------------------------------------------- kinds

/// Closed session-kind vocabulary. `shell` is the only creatable kind on
/// this surface today; unknown kinds (including `pty`) fail decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Shell,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Shell => "shell",
        }
    }
}

/// Data stream labels for shell-session payloads. Two pipes in v1; the
/// merged `pty` label is PTY-surface vocabulary and is out of scope here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStream {
    Stdout,
    Stderr,
}

impl SessionStream {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStream::Stdout => "stdout",
            SessionStream::Stderr => "stderr",
        }
    }
}

// ------------------------------------------------------------ create shape

/// create_session body: one persistent shell per session.
/// `shell` is the guest ABSOLUTE interpreter path (the ExecSpec shell rule:
/// never resolved on the host, never defaulted behind the client's back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    pub kind: SessionKind,
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl SessionSpec {
    /// Semantic validation layered on top of serde's structural decode:
    /// absolute NUL-free guest paths, exec-schema env rules (name non-empty,
    /// no `=`, no NUL; values NUL-free).
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_guest_path("shell", &self.shell)?;
        if let Some(cwd) = &self.cwd {
            validate_guest_path("cwd", cwd)?;
        }
        validate_env(&self.env)
    }
}

fn validate_guest_path(what: &str, raw: &str) -> Result<(), DomainError> {
    if raw.is_empty() {
        return Err(DomainError::invalid(format!("{what} must not be empty")));
    }
    if raw.contains('\0') {
        return Err(DomainError::invalid(format!("{what} contains NUL")));
    }
    if !raw.starts_with('/') {
        return Err(DomainError::invalid(format!(
            "{what} must be an absolute guest path"
        )));
    }
    Ok(())
}

fn validate_env(env: &BTreeMap<String, String>) -> Result<(), DomainError> {
    for (k, v) in env {
        if k.is_empty() || k.contains('=') || k.contains('\0') {
            return Err(DomainError::invalid(format!(
                "env name {k:?} is invalid (non-empty, no '=', no NUL)"
            )));
        }
        if v.contains('\0') {
            return Err(DomainError::invalid(format!("env value for {k:?} contains NUL")));
        }
    }
    Ok(())
}

/// Reject `null` for any top-level field (null != missing, the frozen
/// discipline) before serde fills defaults.
fn reject_explicit_nulls(doc: &Value) -> Result<&serde_json::Map<String, Value>, DomainError> {
    let obj = doc
        .as_object()
        .ok_or_else(|| DomainError::invalid("session document must be a JSON object"))?;
    for (k, v) in obj {
        if v.is_null() {
            return Err(DomainError::invalid(format!(
                "field {k:?} must not be null (null != missing)"
            )));
        }
    }
    Ok(obj)
}

fn invalid(why: impl Into<String>) -> DomainError {
    DomainError::new(ErrorCode::InvalidArgument, why)
}

/// POST .../sessions body → validated {@link SessionSpec}.
pub fn parse_create_body(doc: &Value) -> Result<SessionSpec, DomainError> {
    reject_explicit_nulls(doc)?;
    let spec: SessionSpec =
        serde_json::from_value(doc.clone()).map_err(|e| invalid(format!("create_session: {e}")))?;
    spec.validate()?;
    Ok(spec)
}

/// Client→server start shape; `sandbox_id` is ROUTE material, never a body
/// key (same discipline as commands::StartExecRequest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSessionRequest {
    pub sandbox_id: String,
    pub spec: SessionSpec,
}

pub fn create_request(doc: &Value, sandbox_id: &str) -> Result<CreateSessionRequest, DomainError> {
    if sandbox_id.is_empty() || sandbox_id.contains('\0') {
        return Err(invalid(
            "create_session sandbox_id must be non-empty and NUL-free",
        ));
    }
    Ok(CreateSessionRequest {
        sandbox_id: sandbox_id.to_string(),
        spec: parse_create_body(doc)?,
    })
}

// ---------------------------------------------------------------- run body

/// run_in_session body: exactly one `script` key — shell INPUT fed verbatim
/// to the persistent interpreter's stdin (a persistent shell is never
/// re-exec'd or simulated by command-history replay).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInSessionRequest {
    pub session_id: SessionId,
    pub script: String,
}

pub fn parse_run_body(doc: &Value) -> Result<String, DomainError> {
    let obj = reject_explicit_nulls(doc)?;
    for k in obj.keys() {
        if k != "script" {
            return Err(invalid(format!(
                "unknown field {k:?} for run_in_session (strict whitelist)"
            )));
        }
    }
    let script = obj
        .get("script")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("field \"script\" is required and must be a string"))?;
    if script.is_empty() {
        return Err(invalid("script must not be empty"));
    }
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(invalid(format!(
            "script of {} bytes exceeds {} cap",
            script.len(),
            MAX_SCRIPT_BYTES
        )));
    }
    if script.contains('\0') {
        return Err(invalid("script contains NUL"));
    }
    Ok(script.to_string())
}

pub fn run_request(doc: &Value, session_id: &str) -> Result<RunInSessionRequest, DomainError> {
    Ok(RunInSessionRequest {
        session_id: SessionId::new(session_id)?,
        script: parse_run_body(doc)?,
    })
}

// ---------------------------------------------------------------- ref wire

/// Closed session-state wire vocabulary (03_DOMAIN Session lifecycle;
/// `closed` is the explicit post-cleanup terminal, distinct from the
/// shell-died `exited`).
pub fn session_state_wire(s: &str) -> Result<&'static str, DomainError> {
    match s {
        "open" => Ok("open"),
        "exited" => Ok("exited"),
        "closed" => Ok("closed"),
        other => Err(DomainError::new(
            ErrorCode::InternalFailure,
            format!("unknown session state wire value {other:?}"),
        )),
    }
}

/// Server→client session reference: id + sandbox binding + fencing
/// generation + kind + state (03_DOMAIN: separately created/attached,
/// never equivalent to a per-exec fresh shell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRefWire {
    pub session_id: SessionId,
    pub sandbox_id: String,
    pub generation: u64,
    pub kind: SessionKind,
    pub state: String,
}

pub fn session_ref(
    session_id: &SessionId,
    sandbox_id: &str,
    generation: u64,
    state: &str,
) -> Result<SessionRefWire, DomainError> {
    Ok(SessionRefWire {
        session_id: session_id.clone(),
        sandbox_id: sandbox_id.to_string(),
        generation,
        kind: SessionKind::Shell,
        state: session_state_wire(state)?.to_string(),
    })
}

// ----------------------------------------------------------------- payload

/// One PAYLOAD chunk on the data channel: opaque bytes (base64 on the wire,
/// byte-exact across non-UTF8) + stream + producer offset. This type is the
/// whole point of the control/payload split: it carries NO control meaning
/// and there is no function here that interprets its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodedChunk {
    pub session_id: SessionId,
    pub stream: SessionStream,
    pub offset: u64,
    pub payload_b64: String,
}

/// Encode raw producer bytes into a payload chunk. Fail-closed cap: a
/// bigger slice must be split by the producer, never truncated here.
pub fn encode_chunk(
    session_id: &SessionId,
    stream: SessionStream,
    offset: u64,
    bytes: &[u8],
) -> Result<EncodedChunk, DomainError> {
    if bytes.len() > MAX_CHUNK_BYTES {
        return Err(invalid(format!(
            "chunk of {} bytes exceeds {} cap",
            bytes.len(),
            MAX_CHUNK_BYTES
        )));
    }
    Ok(EncodedChunk {
        session_id: session_id.clone(),
        stream,
        offset,
        payload_b64: base64::encode(bytes),
    })
}

/// Strict-parse a chunk document (deny_unknown_fields + null != missing),
/// then strict-decode the base64 payload. Every failure is INVALID_ARGUMENT.
pub fn parse_chunk(doc: &Value) -> Result<EncodedChunk, DomainError> {
    reject_explicit_nulls(doc)?;
    let chunk: EncodedChunk =
        serde_json::from_value(doc.clone()).map_err(|e| invalid(format!("chunk: {e}")))?;
    chunk_bytes(&chunk)?;
    Ok(chunk)
}

/// Decode the payload back to raw bytes (strict RFC4648-with-padding).
pub fn chunk_bytes(chunk: &EncodedChunk) -> Result<Vec<u8>, DomainError> {
    base64::decode(&chunk.payload_b64)
        .map_err(|e| invalid(format!("chunk payload is not strict base64: {e}")))
}

// ----------------------------------------------------------------- control

/// The shell's observed post-command environment, reported BY THE CONTROL
/// CHANNEL (the worker-side interpreter loop dumps it after each command —
/// the host never scans output for `cd`/`export` effects). Applying it is a
/// full replacement of the persisted view; carrying it is optional per
/// command, absent means "no observation", never "fabricate previous".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedState {
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

impl ObservedState {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_guest_path("observed cwd", &self.cwd)?;
        validate_env(&self.env)
    }
}

/// CONTROL-channel vocabulary: the ONLY channel on which command lifecycle
/// and exit facts may travel. Payload bytes can never produce these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlFrame {
    /// The worker started running queued command `seq`.
    CommandStarted { seq: u64 },
    /// Command `seq` settled; `exit` is the mutually-exclusive exit union
    /// reusing commands::ExitWire (unknown is the honest lost marker).
    CommandFinished {
        seq: u64,
        exit: ExitWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed: Option<ObservedState>,
    },
    /// The persistent shell itself died (its own wait status, NOT a
    /// per-command code). Every live handle is invalidated by this frame.
    SessionExited { exit: ExitWire },
}

/// Decode one control-channel document. Structural failures (unknown kind,
/// unknown field, wrong scalar type, explicit null, bad session id) and
/// semantic failures (illegal observed env/cwd) are all INVALID_ARGUMENT.
pub fn decode_control_frame(doc: &Value) -> Result<ControlFrame, DomainError> {
    reject_explicit_nulls(doc)?;
    let frame: ControlFrame =
        serde_json::from_value(doc.clone()).map_err(|e| invalid(format!("control frame: {e}")))?;
    if let ControlFrame::CommandFinished {
        observed: Some(obs),
        ..
    } = &frame
    {
        obs.validate()?;
    }
    Ok(frame)
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sid() -> SessionId {
        SessionId::new("ses_01").unwrap()
    }

    // ---------------------------------------------------------- create body

    #[test]
    fn create_body_defaults_and_validates() {
        let spec = parse_create_body(&json!({"kind": "shell", "shell": "/bin/bash"})).unwrap();
        assert_eq!(spec.kind, SessionKind::Shell);
        assert!(spec.cwd.is_none());
        assert!(spec.env.is_empty());
        let full = parse_create_body(&json!({
            "kind": "shell", "shell": "/bin/sh", "cwd": "/app", "env": {"A": "1"}
        }))
        .unwrap();
        assert_eq!(full.cwd.as_deref(), Some("/app"));
        assert_eq!(full.env.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn create_body_rejects_unknown_fields_kinds_and_illegal_values() {
        let bad = [
            json!({"kind": "shell", "shell": "/bin/sh", "extra": 1}),       // unknown field
            json!({"kind": "pty", "shell": "/bin/sh"}),                     // kind reserved
            json!({"kind": "PTY", "shell": "/bin/sh"}),                     // case matters
            json!({"kind": "shell", "shell": "sh"}),                        // not absolute
            json!({"kind": "shell", "shell": "/bin/sh", "cwd": "relative"}),
            json!({"kind": "shell", "shell": "/bin/sh", "cwd": null}),      // null != missing
            json!({"kind": "shell", "shell": "/bin/sh", "env": {"A=B": "x"}}),
            json!({"kind": "shell", "shell": "/bin/sh", "env": {"": "x"}}),
            json!({"kind": "shell", "shell": "/bin/sh", "env": {"A": "B\0C"}}),
            json!({"kind": "shell", "shell": "/bin/\0sh"}),
            json!({"kind": "shell"}),                                       // shell required
            json!("not an object"),
        ];
        for doc in bad {
            let err = parse_create_body(&doc).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{doc}");
        }
    }

    #[test]
    fn create_body_golden_json_shape_is_stable() {
        // BTreeMap env gives deterministic key order; pinch the exact wire.
        let spec = SessionSpec {
            kind: SessionKind::Shell,
            shell: "/bin/sh".to_string(),
            cwd: Some("/app".to_string()),
            env: [("Z".to_string(), "9".to_string()), ("A".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
        };
        let text = serde_json::to_string(&spec).unwrap();
        assert_eq!(
            text,
            r#"{"kind":"shell","shell":"/bin/sh","cwd":"/app","env":{"A":"1","Z":"9"}}"#
        );
        let back: SessionSpec = serde_json::from_str(&text).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn route_material_is_boundary_checked() {
        let body = json!({"kind": "shell", "shell": "/bin/sh"});
        assert!(create_request(&body, "").is_err());
        assert!(create_request(&body, "sb\0").is_err());
        let ok = create_request(&body, "sbx-1").unwrap();
        assert_eq!(ok.sandbox_id, "sbx-1");
        assert!(run_request(&json!({"script": "true"}), "").is_err());
        assert!(run_request(&json!({"script": "true"}), "bad id!").is_err());
    }

    // ------------------------------------------------------------- run body

    #[test]
    fn run_body_script_rules_closed() {
        assert_eq!(
            parse_run_body(&json!({"script": "cd /app && pytest -q"})).unwrap(),
            "cd /app && pytest -q"
        );
        let too_long = "x".repeat(MAX_SCRIPT_BYTES + 1);
        let bad = [
            json!({}),
            json!({"script": ""}),
            json!({"script": 42}),
            json!({"script": null}),
            json!({"script": "ls", "extra": true}),
            json!({"script": format!("rm{}\0-rf", "")}),
            json!({"script": too_long}),
        ];
        for doc in bad {
            let err = parse_run_body(&doc).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{doc}");
        }
        // A trailing newline is ordinary shell input, not a protocol marker.
        assert_eq!(parse_run_body(&json!({"script": "ls\n"})).unwrap(), "ls\n");
    }

    // ------------------------------------------------------------------- id

    #[test]
    fn session_id_mirrors_domain_id_rules() {
        let id = SessionId::new("ses_01.A-b").unwrap();
        assert_eq!(id.as_str(), "ses_01.A-b");
        let text = serde_json::to_string(&id).unwrap();
        assert_eq!(text, "\"ses_01.A-b\"");
        let back: SessionId = serde_json::from_str(&text).unwrap();
        assert_eq!(back, id);
        for raw in [
            "",
            &"x".repeat(MAX_ID_LEN + 1),
            "has space",
            "has/slash",
            "nul\0byte",
            "沙盒",
        ] {
            let err = SessionId::new(raw).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{raw:?}");
        }
    }

    // ------------------------------------------------------------------ ref

    #[test]
    fn session_ref_wire_round_trip_and_closed_state_vocabulary() {
        let r = session_ref(&sid(), "sbx-1", 3, "open").unwrap();
        let text = serde_json::to_string(&r).unwrap();
        let back: SessionRefWire = serde_json::from_str(&text).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.generation, 3);
        for ok in ["open", "exited", "closed"] {
            assert_eq!(session_state_wire(ok).unwrap(), ok);
        }
        assert!(session_state_wire("running").is_err());
        assert!(session_ref(&sid(), "sbx-1", 1, "zombie").is_err());
    }

    // --------------------------------------------------------------- chunks

    #[test]
    fn chunk_keeps_non_utf8_payload_byte_exact() {
        let raw: Vec<u8> = vec![0xff, 0xfe, b'a', 0x80, 0x00, 0x7f];
        let chunk = encode_chunk(&sid(), SessionStream::Stderr, 512, &raw).unwrap();
        let text = serde_json::to_string(&chunk).unwrap();
        let parsed = parse_chunk(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(parsed, chunk);
        assert_eq!(parsed.stream, SessionStream::Stderr);
        assert_eq!(parsed.offset, 512);
        assert_eq!(chunk_bytes(&parsed).unwrap(), raw);
    }

    #[test]
    fn chunk_rejects_oversize_bad_base64_and_wrong_shape() {
        assert!(encode_chunk(&sid(), SessionStream::Stdout, 0, &vec![0; MAX_CHUNK_BYTES + 1]).is_err());
        let good = serde_json::to_value(encode_chunk(&sid(), SessionStream::Stdout, 0, b"x").unwrap()).unwrap();
        let mut extra = good.clone();
        extra.as_object_mut().unwrap().insert("e".into(), json!(1));
        let mut bad_stream = good.clone();
        *bad_stream.get_mut("stream").unwrap() = json!("pty"); // reserved, closed
        let mut bad_b64 = good.clone();
        *bad_b64.get_mut("payload_b64").unwrap() = json!("!!!not-b64!!!".trim_end_matches('!'));
        let mut bad_off = good.clone();
        *bad_off.get_mut("offset").unwrap() = json!(true); // bool is not a number
        for doc in [extra, bad_stream, bad_off] {
            let err = parse_chunk(&doc).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{doc}");
        }
        // strict base64: whitespace and missing padding are rejections
        let mut ws = good.clone();
        *ws.get_mut("payload_b64").unwrap() = json!("eA ==");
        assert!(parse_chunk(&ws).is_err());
    }

    // -------------------------------------------------------------- control

    #[test]
    fn control_frame_round_trips_every_variant() {
        let frames = [
            ControlFrame::CommandStarted { seq: 7 },
            ControlFrame::CommandFinished {
                seq: 7,
                exit: ExitWire::Exited { exit_code: 3 },
                observed: Some(ObservedState {
                    cwd: "/app".into(),
                    env: [("A".to_string(), "1".to_string())].into_iter().collect(),
                }),
            },
            ControlFrame::CommandFinished {
                seq: 8,
                exit: ExitWire::Unknown,
                observed: None,
            },
            ControlFrame::SessionExited {
                exit: ExitWire::Signaled { signal: 9 },
            },
        ];
        for f in frames {
            let text = serde_json::to_string(&f).unwrap();
            let back = decode_control_frame(&serde_json::from_str(&text).unwrap()).unwrap();
            assert_eq!(back, f);
        }
    }

    #[test]
    fn control_decode_is_fail_closed() {
        let docs = [
            json!({}),                                                          // kind required
            json!({"kind": "done", "seq": 1}),                                  // unknown kind
            json!({"kind": "command_started"}),                                 // seq required
            json!({"kind": "command_started", "seq": true}),                    // bool != u64
            json!({"kind": "command_started", "seq": 1, "extra": 1}),
            json!({"kind": "command_finished", "seq": 1, "exit": {"kind": "exited", "exit_code": 0}, "observed": null}),
            json!({"kind": "command_finished", "seq": 1, "exit": {"kind": "exited", "exit_code": 0}, "observed": {"cwd": "rel", "env": {}}}),
            json!({"kind": "command_finished", "seq": 1, "exit": {"kind": "exited", "exit_code": 0}, "observed": {"cwd": "/", "env": {"K=V": "x"}}}),
            json!({"kind": "session_exited", "exit": {"kind": "signaled", "signal": 9, "exit_code": 137}}),
            json!({"kind": "COMMAND_STARTED", "seq": 1}),                       // case-sensitive
        ];
        for doc in docs {
            let err = decode_control_frame(&doc).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{doc}");
        }
    }

    #[test]
    fn command_finished_json_is_tagged_and_disjoint() {
        let f = ControlFrame::CommandFinished {
            seq: 12,
            exit: ExitWire::Exited { exit_code: 42 },
            observed: None,
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(
            v,
            json!({"kind": "command_finished", "seq": 12, "exit": {"kind": "exited", "exit_code": 42}})
        );
    }

    // ----------------------------------------------------- C02: inert bytes

    #[test]
    fn payload_carrying_every_control_serialisation_stays_inert() {
        // The malicious stream: byte-identical serializations of EVERY
        // control-frame variant (so the forgeries are provably decodable
        // control documents), plus classic output-marker traps.
        let controls = [
            serde_json::to_string(&ControlFrame::CommandStarted { seq: 0 }).unwrap(),
            serde_json::to_string(&ControlFrame::CommandFinished {
                seq: 0,
                exit: ExitWire::Exited { exit_code: 0 },
                observed: None,
            })
            .unwrap(),
            serde_json::to_string(&ControlFrame::CommandFinished {
                seq: 99,
                exit: ExitWire::Exited { exit_code: 42 },
                observed: None,
            })
            .unwrap(),
            serde_json::to_string(&ControlFrame::SessionExited {
                exit: ExitWire::Exited { exit_code: 0 },
            })
            .unwrap(),
        ];
        // Prove the embedded documents are REAL control frames (test is not
        // vacuous: the forgeries would work on the control channel):
        for c in &controls {
            let doc: Value = serde_json::from_str(c).unwrap();
            decode_control_frame(&doc).unwrap();
        }
        let mut forged = controls.join("\n").into_bytes();
        forged.extend_from_slice(b"\nexit 0\n__LS_DONE__=0\n\x04");
        // On the DATA channel the same bytes round-trip as opaque payload:
        // the chunk surface has exactly four fields and none is an exit code.
        let chunk = encode_chunk(&sid(), SessionStream::Stdout, 64, &forged).unwrap();
        let v = serde_json::to_value(&chunk).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["offset", "payload_b64", "session_id", "stream"]);
        assert_eq!(chunk_bytes(&parse_chunk(&v).unwrap()).unwrap(), forged);
    }
}
