//! T08 lifecycle wire vocabulary (plans/tasks/T08.md).
//!
//! The minimal create/inspect/execute/delete vocabulary that the CLI/SDK
//! and the (later) HTTP façade share when talking to the T08 runtime
//! slice. These shapes mirror docs/v5/03_DOMAIN.md state names and the
//! mutually-exclusive exit record; they are PRODUCT vocabulary — nothing
//! here touches the frozen contracts/v1 schemas (T45 owns any freeze).

use ls_core::domain::backend::{BackendPreference, CapabilityRequirements};
use serde::{Deserialize, Serialize};

/// create request (client -> worker). Image is the runnable store ref name
/// (manifest hex), never a tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSandboxApiRequest {
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub requirements: CapabilityRequirements,
    #[serde(default)]
    pub preference: BackendPreference,
}

/// create response. `observed_state` is the KERNEL-observed state right
/// after start (never a self-declared "running").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedSandboxWire {
    pub sandbox_id: String,
    pub generation: u64,
    pub observed_state: String,
    pub backend_kind: String,
    pub plan_reason: String,
}

/// inspect response: record facts PLUS the fresh observation they were
/// reconciled against (both, never one silently swapped for the other).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxInfoWire {
    pub sandbox_id: String,
    pub generation: u64,
    pub revision: u64,
    pub lifetime: String,
    pub state: String,
    pub observed_backend: String,
    pub image_digest: String,
    pub created_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_receipt_exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
}

/// execute request: argv explicit; v1 never guesses a shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecApiRequest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Mutually-exclusive exit record (03_DOMAIN §命令与退出). `UNKNOWN` is
/// allowed ONLY alongside an explicit non-observed state — infrastructure
/// lost the result; never a silent success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExitRecordWire {
    Exited { exit_code: i32 },
    Signaled { signal: u32 },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TerminationReasonWire {
    None,
    Timeout,
    Cancelled,
    OomKilled,
}

/// execute response: synchronous in v1; truncation is explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionOutcomeWire {
    pub execution_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_record: Option<ExitRecordWire>,
    pub termination: TerminationReasonWire,
    pub stdout: String,
    pub stderr: String,
    pub wall_ms: u64,
    pub truncated: bool,
    #[serde(default)]
    pub audits: Vec<String>,
}

/// delete response pointer: the full ledger lives in the runtime's
/// CleanupInfo document (cleanup-id addressable); the wire answer carries
/// acceptance + final disposition separately (DEL-1), never merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteAckWire {
    pub sandbox_id: String,
    pub cleanup_id: String,
    /// "deleted" | "cleanup-required" (the only honest outcomes).
    pub final_state: String,
    pub steps_ok: bool,
    pub failed_steps: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_record_wire_is_mutually_exclusive_and_tagged() {
        let e = ExitRecordWire::Exited { exit_code: 7 };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, "{\"kind\":\"EXITED\",\"exit_code\":7}");
        assert!(serde_json::to_string(&ExitRecordWire::Unknown)
            .unwrap()
            .contains("UNKNOWN"));
        let back: ExitRecordWire = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn timeout_outcome_reports_unknown_never_a_fabricated_code() {
        let out = ExecutionOutcomeWire {
            execution_id: "exec-1".into(),
            state: "unknown".into(),
            exit_record: Some(ExitRecordWire::Unknown),
            termination: TerminationReasonWire::Timeout,
            stdout: String::new(),
            stderr: String::new(),
            wall_ms: 5000,
            truncated: false,
            audits: vec![],
        };
        let s = serde_json::to_string(&out).unwrap();
        assert!(s.contains("\"termination\":\"TIMEOUT\""));
        assert!(s.contains("UNKNOWN"));
        let back: ExecutionOutcomeWire = serde_json::from_str(&s).unwrap();
        assert_eq!(back, out);
    }

    #[test]
    fn create_request_defaults_requirements_and_preference() {
        let req: CreateSandboxApiRequest =
            serde_json::from_str("{\"image\":\"deadbeef\"}").unwrap();
        assert_eq!(req.image, "deadbeef");
        assert!(req.workdir.is_none());
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("workdir"));
    }

    #[test]
    fn delete_ack_never_merges_acceptance_and_ledger() {
        let ack = DeleteAckWire {
            sandbox_id: "sbx-1".into(),
            cleanup_id: "cln-1".into(),
            final_state: "cleanup-required".into(),
            steps_ok: false,
            failed_steps: vec!["lease-teardown".into()],
        };
        let s = serde_json::to_string(&ack).unwrap();
        assert!(s.contains("cleanup-required"));
        assert!(s.contains("lease-teardown"));
        let back: DeleteAckWire = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ack);
    }
}
