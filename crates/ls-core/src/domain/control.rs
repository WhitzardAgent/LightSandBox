//! Typed control protocol: the ONLY messages the unprivileged API may send
//! the privileged manager over its UDS (T04-C03;
//! docs/v5/02_ARCHITECTURE.md §进程和权限).
//!
//! Design guarantees (enforced here, locked by fixtures):
//! * a closed opcode set — anything unregistered is INVALID_ARGUMENT at
//!   parse; there is NO host shell op and NO eval path to fall back to
//! * rootfs/image addressing is by [`BundleRef`] digest only; the protocol
//!   simply has no host-path field — an injected `root`/`hostPath`/
//!   `mount_source` key is an UNKNOWN FIELD and rejected
//! * guest paths are [`GuestPath`]; argv is literal guest execution input
//!   (shell metacharacters inside argv are NOT protocol injection: argv is
//!   never interpreted by any host shell — that contrast is pinned in the
//!   accept fixtures)
//! * strict per-op field whitelists; wrong-typed scalars (bool as number,
//!   negative generation) rejected at parse

use super::error::DomainError;
use super::ids::{BundleRef, ExecutionId, Generation, GuestPath, SandboxId};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelScope {
    Process,
    Tree,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    SandboxCreate {
        sandbox_id: SandboxId,
        bundle: BundleRef,
        workdir: Option<GuestPath>,
        env: BTreeMap<String, String>,
    },
    SandboxDelete {
        sandbox_id: SandboxId,
        generation: Generation,
    },
    ExecutionStart {
        sandbox_id: SandboxId,
        generation: Generation,
        execution_id: ExecutionId,
        argv: Vec<String>,
    },
    ExecutionCancel {
        execution_id: ExecutionId,
        scope: CancelScope,
    },
}

impl ControlRequest {
    pub fn op_name(&self) -> &'static str {
        match self {
            ControlRequest::SandboxCreate { .. } => "sandbox.create",
            ControlRequest::SandboxDelete { .. } => "sandbox.delete",
            ControlRequest::ExecutionStart { .. } => "execution.start",
            ControlRequest::ExecutionCancel { .. } => "execution.cancel",
        }
    }
}

fn require_object<'a>(
    doc: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, DomainError> {
    let obj = doc
        .as_object()
        .ok_or_else(|| DomainError::invalid("control request must be a JSON object"))?;
    for key in obj.keys() {
        if key != "op" && !allowed.contains(&key.as_str()) {
            return Err(DomainError::invalid(format!(
                "unknown field {key:?} for this control op (strict whitelist)"
            )));
        }
    }
    Ok(obj)
}

fn string_field<'a>(obj: &'a serde_json::Map<String, Value>, name: &str) -> Result<&'a str, DomainError> {
    obj.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::invalid(format!("field {name:?} is required and must be a string")))
}

fn generation_field(obj: &serde_json::Map<String, Value>) -> Result<Generation, DomainError> {
    obj.get("generation")
        .and_then(Value::as_u64)
        .map(Generation)
        .ok_or_else(|| DomainError::invalid(
            "field \"generation\" is required and must be a non-negative integer (bools/strings rejected)",
        ))
}

fn no_nul(what: &str, s: &str) -> Result<(), DomainError> {
    if s.chars().any(|c| c == '\0') {
        return Err(DomainError::invalid(format!("{what} contains NUL")));
    }
    Ok(())
}

/// Parse a wire document into a typed request. Every rejection carries
/// INVALID_ARGUMENT and names the offending field/op.
pub fn parse(doc: &Value) -> Result<ControlRequest, DomainError> {
    let op = doc
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::invalid("field \"op\" is required and must be a string"))?;
    match op {
        "sandbox.create" => {
            let obj = require_object(doc, &["sandbox_id", "bundle", "workdir", "env"])?;
            let workdir = obj
                .get("workdir")
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| DomainError::invalid("field \"workdir\" must be a string"))
                        .and_then(GuestPath::new)
                })
                .transpose()?;
            let env = obj
                .get("env")
                .map(|v| {
                    let map = v
                        .as_object()
                        .ok_or_else(|| DomainError::invalid("field \"env\" must be an object"))?;
                    let mut out = BTreeMap::new();
                    for (k, x) in map {
                        let value = x
                            .as_str()
                            .ok_or_else(|| DomainError::invalid(format!("env value for {k:?} must be a string")))?;
                        no_nul("env key", k)?;
                        no_nul("env value", value)?;
                        out.insert(k.clone(), value.to_string());
                    }
                    Ok::<_, DomainError>(out)
                })
                .transpose()?
                .unwrap_or_default();
            Ok(ControlRequest::SandboxCreate {
                sandbox_id: SandboxId::new(string_field(obj, "sandbox_id")?)?,
                bundle: BundleRef::new(string_field(obj, "bundle")?)?,
                workdir,
                env,
            })
        }
        "sandbox.delete" => {
            let obj = require_object(doc, &["sandbox_id", "generation"])?;
            Ok(ControlRequest::SandboxDelete {
                sandbox_id: SandboxId::new(string_field(obj, "sandbox_id")?)?,
                generation: generation_field(obj)?,
            })
        }
        "execution.start" => {
            let obj = require_object(doc, &["sandbox_id", "generation", "execution_id", "argv"])?;
            let raw = obj
                .get("argv")
                .and_then(Value::as_array)
                .ok_or_else(|| DomainError::invalid("field \"argv\" is required and must be an array"))?;
            if raw.is_empty() {
                return Err(DomainError::invalid("argv must not be empty"));
            }
            let mut argv = Vec::with_capacity(raw.len());
            for item in raw {
                let s = item
                    .as_str()
                    .ok_or_else(|| DomainError::invalid("argv elements must be strings"))?;
                if s.is_empty() {
                    return Err(DomainError::invalid("argv elements must not be empty"));
                }
                no_nul("argv element", s)?;
                argv.push(s.to_string());
            }
            Ok(ControlRequest::ExecutionStart {
                sandbox_id: SandboxId::new(string_field(obj, "sandbox_id")?)?,
                generation: generation_field(obj)?,
                execution_id: ExecutionId::new(string_field(obj, "execution_id")?)?,
                argv,
            })
        }
        "execution.cancel" => {
            let obj = require_object(doc, &["execution_id", "scope"])?;
            let scope = match string_field(obj, "scope")? {
                "process" => CancelScope::Process,
                "tree" => CancelScope::Tree,
                "sandbox" => CancelScope::Sandbox,
                other => {
                    return Err(DomainError::invalid(format!(
                        "unknown cancel scope {other:?} (process|tree|sandbox)"
                    )))
                }
            };
            Ok(ControlRequest::ExecutionCancel {
                execution_id: ExecutionId::new(string_field(obj, "execution_id")?)?,
                scope,
            })
        }
        other => Err(DomainError::invalid(format!(
            "unregistered control op {other:?}; the protocol has no host shell and no fallback dispatch"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::repo_file_json;

    #[test]
    fn accept_cases_parse_to_typed_requests() {
        let fx = repo_file_json("tests/unit/contracts/control_requests.json");
        for case in fx["accept"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let req = parse(&case["doc"]).unwrap_or_else(|e| panic!("accept {name}: {e}"));
            assert_eq!(req.op_name(), case["expect_op"].as_str().unwrap(), "{name}");
        }
    }

    #[test]
    fn accept_fields_are_typed_precisely() {
        let fx = repo_file_json("tests/unit/contracts/control_requests.json");
        let cases = fx["accept"].as_array().unwrap();
        let create = cases.iter().find(|c| c["name"] == "create-full").unwrap();
        match parse(&create["doc"]).unwrap() {
            ControlRequest::SandboxCreate {
                sandbox_id,
                bundle,
                workdir,
                env,
            } => {
                assert_eq!(sandbox_id.as_str(), "sb_01");
                assert_eq!(bundle.as_str().len(), 71); // sha256: + 64
                assert_eq!(workdir.unwrap().as_str(), "/app");
                assert_eq!(env.get("A").unwrap(), "B");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let cancel = cases.iter().find(|c| c["name"] == "cancel-tree").unwrap();
        match parse(&cancel["doc"]).unwrap() {
            ControlRequest::ExecutionCancel { scope, .. } => {
                assert_eq!(scope, CancelScope::Tree)
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn reject_cases_fail_with_invalid_argument() {
        let fx = repo_file_json("tests/unit/contracts/control_requests.json");
        for case in fx["reject"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let err = parse(&case["doc"]).unwrap_err();
            assert_eq!(
                err.code,
                super::super::error::ErrorCode::InvalidArgument,
                "reject {name}"
            );
        }
    }

    #[test]
    fn injection_vectors_all_rejected() {
        let fx = repo_file_json("tests/security/control/injection.json");
        for vector in fx["vectors"].as_array().unwrap() {
            let name = vector["name"].as_str().unwrap();
            let err = parse(&vector["doc"]).unwrap_err();
            assert_eq!(
                err.code,
                super::super::error::ErrorCode::InvalidArgument,
                "injection {name}"
            );
        }
    }

    #[test]
    fn unregistered_op_never_reaches_dispatch() {
        // The parse layer is the ONLY entry point; its error type has no
        // "execute anyway" path. An op string that LOOKS registered in some
        // other spelling is still rejected by exact match.
        for spoof in ["Sandbox.Create", "sandbox.create ", " sandbox.create", "sandbox/Create"] {
            let doc = serde_json::json!({
                "op": spoof,
                "sandbox_id": "sb_01",
                "bundle": format!("sha256:{}", "a".repeat(64)),
            });
            assert!(parse(&doc).is_err(), "spoofed op {spoof:?} must reject");
        }
    }

    #[test]
    fn nul_is_rejected_in_all_string_positions() {
        // Real NUL characters cannot ride inside JSON fixtures (the encoding
        // layer rewrites them), so this vector set is built in code. Every
        // string position that crosses the wire must reject NUL.
        let digest = format!("sha256:{}", "a".repeat(64));
        let cases = [
            serde_json::json!({"op": "sandbox.create", "sandbox_id": format!("sb{}", '\0'), "bundle": digest}),
            serde_json::json!({"op": "sandbox.create", "sandbox_id": "sb_01", "bundle": digest, "workdir": format!("/a{}", '\0')}),
            serde_json::json!({"op": "sandbox.create", "sandbox_id": "sb_01", "bundle": digest, "env": {"A": format!("B{}C", '\0')}}),
            serde_json::json!({"op": "execution.start", "sandbox_id": "sb_01", "generation": 1, "execution_id": "ex_01", "argv": [format!("sh{}", '\0')]}),
            serde_json::json!({"op": "execution.cancel", "execution_id": format!("ex{}", '\0'), "scope": "process"}),
        ];
        for (i, doc) in cases.iter().enumerate() {
            let err = parse(doc).unwrap_err();
            assert_eq!(
                err.code,
                super::super::error::ErrorCode::InvalidArgument,
                "nul case {i}"
            );
        }
    }

    #[test]
    fn reject_error_names_understands_field_or_op() {
        let doc = serde_json::json!({"op": "sandbox.delete", "sandbox_id": "sb_01", "generation": 1, "force": true});
        let err = parse(&doc).unwrap_err();
        assert!(err.message.contains("force"), "rejection must name the field: {err}");
    }
}
