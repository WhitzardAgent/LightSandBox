//! Real cleanup records (docs/v5/03_DOMAIN.md §删除/DEL-1).
//!
//! Delete acceptance and the final reclaim are recorded SEPARATELY: the
//! manager persists a `CleanupInfo` document for every deletion with each
//! real step (container teardown, bundle removal, rootfs release incl. its
//! mount-leak verification) and its outcome. Nothing in a step is claimed
//! that was not executed: a step that could not run is `ok:false` with the
//! error in `note` — deletion then lands in CleanupRequired, never in a
//! falsely-clean Deleted.

use serde::{Deserialize, Serialize};

/// One executed cleanup step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupStep {
    pub name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub wall_ms: u64,
}

/// DETAIL of one sandbox reclaim (DEL-1 vocabulary: target, scope,
/// ops-terminated, compaction result, receipt-archive note, cleanup-id,
/// reversible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupInfo {
    /// Server-generated cleanup id (distinct from the sandbox/ops ids).
    pub cleanup_id: String,
    pub target_sandbox_id: String,
    pub target_generation: u64,
    /// v1 scope is always a whole sandbox (never shed network-only etc).
    pub scope: String,
    /// Executions/operations observed in-flight at delete time (v1: execs
    /// are synchronous calls, so this is the count of registered executions
    /// that were not yet terminal when delete started — never claimed zero
    /// when untracked).
    pub ops_terminated: Option<u64>,
    /// The init receipt archived BEFORE the rootfs layer was released
    /// (raw internal receipt JSON), when one existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_init_receipt: Option<String>,
    /// Compaction/result reference (image-cache GC etc) — v1 does not
    /// compact at delete time: null, not a fabricated digest.
    pub compaction_result: Option<String>,
    /// Sandbox teardown is irreversible, always.
    pub reversible: bool,
    pub steps: Vec<CleanupStep>,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
}

impl CleanupInfo {
    /// All steps ok. The caller maps false -> domain CleanupRequired.
    pub fn all_ok(&self) -> bool {
        self.steps.iter().all(|s| s.ok)
    }

    pub fn failed_steps(&self) -> Vec<&CleanupStep> {
        self.steps.iter().filter(|s| !s.ok).collect()
    }
}

pub fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_info_serde_roundtrips_with_null_compaction() {
        let info = CleanupInfo {
            cleanup_id: "cln-1".into(),
            target_sandbox_id: "sbx_1".into(),
            target_generation: 1,
            scope: "sandbox".into(),
            ops_terminated: Some(0),
            archived_init_receipt: None,
            compaction_result: None,
            reversible: false,
            steps: vec![CleanupStep {
                name: "container-delete".into(),
                ok: true,
                note: None,
                wall_ms: 12,
            }],
            started_unix_ms: 1,
            finished_unix_ms: 2,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"compaction_result\":null"));
        assert!(json.contains("\"reversible\":false"));
        let back: CleanupInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
        assert!(back.all_ok());
        assert_eq!(back.failed_steps().len(), 0);
    }
}
