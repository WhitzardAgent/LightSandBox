//! T10 — native v1 exec command wire vocabulary (plans/tasks/T10.md;
//! contracts/v1/exec.schema.json, contracts/v1/surface.json).
//!
//! This module is deliberately a THIN wire layer:
//! * request parsing delegates to the frozen source of truth
//!   `ls_core::contracts::ExecSpec::parse` (strict: null != missing, unknown
//!   fields rejected, kind-tagged argv|shell union) — the same parser the
//!   T41 contract tests pin against contracts/v1/fixtures/execspec;
//! * idempotency digests and the execution state machine live in
//!   `ls_host::exec` (the resolving side needs the sandbox rootfs — user
//!   names resolve within the rootfs, so no digest is computed here);
//! * stdout/stderr travel as BASE64 of the raw bytes (06_HTTP §输出与重放):
//!   the core never substitutes UTF-8, so non-UTF8 payloads round-trip
//!   byte-exact. Cross-chunk decoding lives in the SDK, not here.

use ls_core::contracts::ExecSpec as ContractExecSpec;
use ls_core::domain::error::{DomainError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod base64;

// ---------------------------------------------------------------- request

/// POST /v1/sandboxes/{sandbox_id}/executions + Idempotency-Key header
/// (contracts/v1/surface.json). The body is the frozen exec spec document;
/// `sandbox_id` and the operation key are ROUTE material, never body keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartExecRequest {
    pub sandbox_id: String,
    pub operation_key: String,
    pub spec: ContractExecSpec,
}

/// Parse a raw request body against the frozen exec schema vocabulary.
/// Every rejection is INVALID_ARGUMENT naming the offending field.
pub fn parse_exec_body(doc: &Value) -> Result<ContractExecSpec, DomainError> {
    ContractExecSpec::parse(doc)
        .map_err(|why| DomainError::new(ErrorCode::InvalidArgument, format!("exec spec: {why}")))
}

/// Assemble the full start request. Both route strings are boundary-checked
/// here (non-empty, NUL-free); the idempotency scope's own length limits run
/// at the service boundary (ls-core IdempotencyScope).
pub fn start_request(
    doc: &Value,
    sandbox_id: &str,
    operation_key: &str,
) -> Result<StartExecRequest, DomainError> {
    for (what, v) in [("sandbox_id", sandbox_id), ("operation_key", operation_key)] {
        if v.is_empty() || v.contains('\0') {
            return Err(DomainError::new(
                ErrorCode::InvalidArgument,
                format!("exec {what} must be non-empty and NUL-free"),
            ));
        }
    }
    Ok(StartExecRequest {
        sandbox_id: sandbox_id.to_string(),
        operation_key: operation_key.to_string(),
        spec: parse_exec_body(doc)?,
    })
}

// --------------------------------------------------------------- response

/// The mutually-exclusive exit union on the wire (03_DOMAIN §命令与退出).
/// `exit_code` and `signal` never coexist; `Unknown` is the explicit
/// infrastructure-lost marker, never a silent success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ExitWire {
    Exited { exit_code: i32 },
    Signaled { signal: u32 },
    Unknown,
}

/// Terminal-state exec operation object (06_HTTP §命令与退出 response).
/// Execution states use the frozen kebab-case wire values
/// ("accepted"/"starting"/"running"/"exited"/"start-failed"/"unknown") —
/// they are produced by ExecutionState::wire() upstream and only carried
/// here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOperationWire {
    pub execution_id: String,
    pub sandbox_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitWire>,
    /// frozen TerminationReason wire value ("none"|"timeout"|"cancelled"|"oomkilled").
    pub termination: String,
    /// base64(RAW BYTES) — byte-exact across non-UTF8 payloads.
    pub stdout_b64: String,
    pub stderr_b64: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub wall_ms: u64,
    /// true when this object answers an idempotent REPLAY (same scope +
    /// same digest); the effect ran exactly once at first acceptance.
    pub replayed: bool,
}

/// Frozen TerminationReason wire strings (state wire values come from the
/// state machine itself; the reason vocabulary is pinned here).
pub fn termination_wire(r: &str) -> Result<&'static str, DomainError> {
    match r {
        "none" => Ok("none"),
        "timeout" => Ok("timeout"),
        "cancelled" => Ok("cancelled"),
        "oomkilled" => Ok("oomkilled"),
        other => Err(DomainError::new(
            ErrorCode::InternalFailure,
            format!("unknown termination wire value {other:?}"),
        )),
    }
}

/// Build the terminal response object. Raw stdout/stderr go through base64
/// verbatim — this is THE point where byte fidelity is locked onto the wire.
pub fn operation_wire(
    execution_id: &str,
    sandbox_id: &str,
    state: &str,
    exit: Option<ExitWire>,
    termination: &str,
    stdout: &[u8],
    stderr: &[u8],
    stdout_truncated: bool,
    stderr_truncated: bool,
    wall_ms: u64,
    replayed: bool,
) -> Result<ExecOperationWire, DomainError> {
    Ok(ExecOperationWire {
        execution_id: execution_id.to_string(),
        sandbox_id: sandbox_id.to_string(),
        state: state.to_string(),
        exit,
        termination: termination_wire(termination)?.to_string(),
        stdout_b64: base64::encode(stdout),
        stderr_b64: base64::encode(stderr),
        stdout_truncated,
        stderr_truncated,
        wall_ms,
        replayed,
    })
}

// ------------------------------------------------------------------ errors

/// Frozen error envelope (06_HTTP §统一错误): the machine code is the
/// ls-core ErrorCode wire string, http_status its frozen mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorWire {
    pub code: String,
    pub message: String,
    pub http_status: u16,
}

pub fn error_wire(err: &DomainError) -> ErrorWire {
    ErrorWire {
        code: err.code.wire().to_string(),
        message: err.to_string(),
        http_status: err.code.http_status(),
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    // Every frozen execspec case must be accepted/rejected EXACTLY as the
    // contract says; ACCEPT cases additionally pin the normalized view
    // (x-rule in the fixture header: every engine must agree).
    #[test]
    fn frozen_execspec_cases_all_agree() {
        let fx = ls_core::contracts::fixture_json("fixtures/execspec/cases.json");
        let cases = fx["cases"].as_array().unwrap();
        assert!(cases.len() >= 20, "fixture must keep its full denominator");
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let expect = case["expect"].as_str().unwrap();
            let got = parse_exec_body(&case["document"]);
            match expect {
                "ACCEPT" => {
                    let spec = got.unwrap_or_else(|e| panic!("{name}: {e}"));
                    let norm = &case["normalized"];
                    if let Some(argv) = norm.get("argv") {
                        let ls_core::contracts::Command::Argv(a) = &spec.command else {
                            panic!("{name}: normalized argv but spec is {:?}", spec.command)
                        };
                        assert_eq!(
                            &serde_json::to_value(a).unwrap(),
                            argv,
                            "{name}: normalized argv"
                        );
                    }
                    if let Some(exe) = norm.get("executable") {
                        let ls_core::contracts::Command::Shell { executable, script } =
                            &spec.command
                        else {
                            panic!("{name}: normalized shell but spec is {:?}", spec.command)
                        };
                        assert_eq!(executable, exe.as_str().unwrap(), "{name}");
                        assert_eq!(
                            script,
                            norm["script"].as_str().unwrap(),
                            "{name}: normalized script"
                        );
                    }
                    if let Some(env) = norm.get("env") {
                        let ours: serde_json::Map<String, Value> = spec
                            .env
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        assert_eq!(
                            &Value::Object(ours),
                            env,
                            "{name}: normalized env"
                        );
                    }
                    if let Some(cwd) = norm.get("cwd") {
                        assert_eq!(
                            spec.cwd.as_deref(),
                            Some(cwd.as_str().unwrap()),
                            "{name}: normalized cwd"
                        );
                    }
                }
                "REJECT" => {
                    let err = got.unwrap_err();
                    assert_eq!(err.code, ErrorCode::InvalidArgument, "{name}");
                }
                other => panic!("{name}: unexpected expect {other}"),
            }
        }
    }

    #[test]
    fn route_material_is_boundary_checked() {
        let body = serde_json::json!({"command": {"kind": "argv", "argv": ["/bin/true"]}});
        assert!(start_request(&body, "", "k").is_err());
        assert!(start_request(&body, "sb", "").is_err());
        assert!(start_request(&body, "sb\0", "k").is_err());
        let ok = start_request(&body, "sbx-1", "K-1").unwrap();
        assert_eq!(ok.sandbox_id, "sbx-1");
        assert_eq!(ok.operation_key, "K-1");
    }

    #[test]
    fn operation_wire_keeps_non_utf8_bytes_exact() {
        // The whole point: non-UTF8 payloads must cross the wire byte-exact.
        let raw_out: Vec<u8> = vec![0xff, 0xfe, b'a', 0x80, 0x00, 0x7f];
        let raw_err: Vec<u8> = vec![b'x', 0xf0, 0x28, 0x8c, 0x28]; // invalid UTF-8 seq
        let w = operation_wire(
            "exec-1",
            "sbx-1",
            "exited",
            Some(ExitWire::Exited { exit_code: 7 }),
            "none",
            &raw_out,
            &raw_err,
            false,
            true,
            42,
            false,
        )
        .unwrap();
        assert_eq!(base64::decode(&w.stdout_b64).unwrap(), raw_out);
        assert_eq!(base64::decode(&w.stderr_b64).unwrap(), raw_err);
        assert!(w.stderr_truncated);
        assert_eq!(w.wall_ms, 42);
        // serde round-trip keeps the union disjoint.
        let text = serde_json::to_string(&w).unwrap();
        let back: ExecOperationWire = serde_json::from_str(&text).unwrap();
        assert_eq!(back, w);
        assert!(matches!(back.exit, Some(ExitWire::Exited { exit_code: 7 })));
    }

    #[test]
    fn exit_union_is_mutually_exclusive_on_wire() {
        let exited = serde_json::to_value(ExitWire::Exited { exit_code: 0 }).unwrap();
        assert_eq!(exited, serde_json::json!({"kind": "exited", "exit_code": 0}));
        let sig = serde_json::to_value(ExitWire::Signaled { signal: 11 }).unwrap();
        assert_eq!(sig, serde_json::json!({"kind": "signaled", "signal": 11}));
        let unknown = serde_json::to_value(ExitWire::Unknown).unwrap();
        assert_eq!(unknown, serde_json::json!({"kind": "unknown"}));
        // a signaled frame carrying exit_code must NOT decode
        assert!(serde_json::from_value::<ExitWire>(
            serde_json::json!({"kind": "signaled", "signal": 11, "exit_code": 139})
        )
        .is_err());
    }

    #[test]
    fn error_wire_uses_frozen_codes_and_status() {
        let e = DomainError::new(ErrorCode::PolicyUnsupported, "non-root exec user");
        let w = error_wire(&e);
        assert_eq!(w.code, "POLICY_UNSUPPORTED");
        assert_eq!(w.http_status, 412);
        let e = DomainError::new(ErrorCode::Conflict, "idempotency key reused");
        assert_eq!(error_wire(&e).http_status, 409);
        let e = DomainError::new(ErrorCode::NotFound, "no such execution");
        assert_eq!(error_wire(&e).code, "NOT_FOUND");
    }

    #[test]
    fn termination_wire_vocabulary_is_closed() {
        assert_eq!(termination_wire("none").unwrap(), "none");
        assert_eq!(termination_wire("oomkilled").unwrap(), "oomkilled");
        assert!(termination_wire("killed").is_err());
    }
}
