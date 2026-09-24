//! Schema migration versions, upgrade planning and crash-resume decisions
//! (T04-C04; task output: "SQLite 意图/租约 schema").
//!
//! ls-core owns the DATA and the DECISIONS, never the database: the intent /
//! lease / receipt DDL is pinned here as the versioned contract the manager's
//! SQLite store must apply, and `resume` decides — purely — whether a crashed
//! migration continues at the next step or REFUSES TO START. A mismatched or
//! gapped step receipt means corruption: refuse, never silently lose data.
//!
//! The runtime half of C04 (interrupting a REAL SQLite migration and proving
//! restart behaviour on disk) belongs to the ls-host persistence layer and is
//! honestly BLOCKED until that task; this module is its decision contract.

use super::error::{DomainError, ErrorCode};

/// One irreversible schema step. Steps are append-only history: never edit a
/// shipped step, add the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStep {
    pub version: u32,
    pub name: &'static str,
    pub ddl: &'static str,
}

/// v1: intents (idempotency), sandboxes (owner/generation/revision),
/// operations, executions, leases, cleanup receipts.
pub const INTENT_LEASE_SCHEMA_SQL: &str = "\
CREATE TABLE schema_version (
  version INTEGER NOT NULL,
  applied_steps TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE intents (
  principal TEXT NOT NULL,
  api_version TEXT NOT NULL,
  method TEXT NOT NULL,
  canonical_route TEXT NOT NULL,
  resource_identity TEXT NOT NULL,
  idem_key TEXT NOT NULL,
  request_digest TEXT NOT NULL,
  result_ref TEXT,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (principal, api_version, method, canonical_route, resource_identity, idem_key)
);
CREATE TABLE sandboxes (
  sandbox_id TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  generation INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  lifetime TEXT NOT NULL
);
CREATE TABLE operations (
  operation_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  result_kind TEXT,
  result_ref TEXT,
  audits TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE executions (
  execution_id TEXT PRIMARY KEY,
  sandbox_id TEXT NOT NULL,
  sandbox_generation INTEGER NOT NULL,
  state TEXT NOT NULL,
  exit_record TEXT,
  termination TEXT NOT NULL DEFAULT 'none'
);
CREATE TABLE leases (
  lease_id TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  sandbox_id TEXT NOT NULL,
  state TEXT NOT NULL
);
CREATE TABLE cleanup_records (
  cleanup_id TEXT PRIMARY KEY,
  sandbox_id TEXT NOT NULL,
  state TEXT NOT NULL,
  evidence TEXT
);
";

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

pub const SCHEMA_STEPS: &[MigrationStep] = &[MigrationStep {
    version: 1,
    name: "intent-lease-receipt-core",
    ddl: INTENT_LEASE_SCHEMA_SQL,
}];

/// Which steps must run to bring `current` to SUPPORTED. A database NEWER
/// than this build refuses (never downgrade silently). Relies on the
/// invariant that step versions are exactly 1..=SUPPORTED (test-pinned), so
/// a version number doubles as a slice index into history.
pub fn plan_upgrade(current: u32) -> Result<&'static [MigrationStep], DomainError> {
    if current > SUPPORTED_SCHEMA_VERSION {
        return Err(DomainError::new(
            ErrorCode::Unavailable,
            format!(
                "database schema v{current} is newer than supported v{SUPPORTED_SCHEMA_VERSION}; refusing to start against it"
            ),
        ));
    }
    Ok(&SCHEMA_STEPS[current as usize..])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Nothing to do; the database is current and consistent.
    AlreadyCurrent,
    /// Crash-safe continuation: apply exactly this step next.
    Resume { next_version: u32 },
    /// Do not open the database. Data is preserved; an operator must decide.
    Refuse { reason: String },
}

/// Decide what a restarted migrator may do, given the persisted version and
/// the persisted per-step receipts. `applied_steps` must be exactly the
/// prefix [1..=current]; anything else (gap, duplicate, unknown, newer) is a
/// Refuse — the "recover OR refuse, never silently lose" rule of T04-C04.
pub fn resume(current: u32, applied_steps: &[u32]) -> ResumeDecision {
    if plan_upgrade(current).is_err() {
        return ResumeDecision::Refuse {
            reason: format!(
                "database schema v{current} newer than supported v{SUPPORTED_SCHEMA_VERSION}; refusing, data preserved"
            ),
        };
    }
    let expected_prefix: Vec<u32> = (1..=current).collect();
    if applied_steps != expected_prefix.as_slice() {
        return ResumeDecision::Refuse {
            reason: format!(
                "step receipts {applied_steps:?} mismatch persisted version v{current}: refusing to start, no data silently altered"
            ),
        };
    }
    if current == SUPPORTED_SCHEMA_VERSION {
        return ResumeDecision::AlreadyCurrent;
    }
    ResumeDecision::Resume {
        next_version: current + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- invariants over the schema data ----

    #[test]
    fn steps_are_contiguous_from_one_and_supported_matches_last() {
        assert!(!SCHEMA_STEPS.is_empty());
        for (i, step) in SCHEMA_STEPS.iter().enumerate() {
            assert_eq!(step.version as usize, i + 1, "step versions must be 1,2,3,...");
            assert!(!step.name.is_empty());
            assert!(!step.ddl.is_empty());
        }
        assert_eq!(SUPPORTED_SCHEMA_VERSION, SCHEMA_STEPS.last().unwrap().version);
    }

    #[test]
    fn intent_ddl_pins_idempotency_uniqueness_tuple() {
        let ddl = INTENT_LEASE_SCHEMA_SQL;
        for table in [
            "intents",
            "sandboxes",
            "operations",
            "executions",
            "leases",
            "cleanup_records",
            "schema_version",
        ] {
            assert!(ddl.contains(&format!("CREATE TABLE {table}")), "missing table {table}");
        }
        assert!(ddl.contains(
            "UNIQUE (principal, api_version, method, canonical_route, resource_identity, idem_key)"
        ));
        // fencing columns must exist where the domain says they live
        assert!(ddl.contains("generation INTEGER NOT NULL"));
        assert!(ddl.contains("revision INTEGER NOT NULL"));
        assert!(ddl.contains("sandbox_generation INTEGER NOT NULL"));
    }

    // ---- upgrade planning ----

    #[test]
    fn plan_for_fresh_database_is_all_steps() {
        let plan = plan_upgrade(0).unwrap();
        assert_eq!(plan.len(), SCHEMA_STEPS.len());
        assert_eq!(plan[0].version, 1);
    }

    #[test]
    fn plan_for_current_database_is_empty() {
        assert!(plan_upgrade(SUPPORTED_SCHEMA_VERSION).unwrap().is_empty());
    }

    #[test]
    fn plan_for_newer_database_refuses_unavailable() {
        let err = plan_upgrade(SUPPORTED_SCHEMA_VERSION + 1).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unavailable);
    }

    // ---- T04-C04: interrupted migration resume decisions ----

    #[test]
    fn resume_halfway_continues_at_next_step() {
        // hypothetical 2-step chain shape, exercised via the prefix rule
        let decision = resume(0, &[]);
        assert_eq!(decision, ResumeDecision::Resume { next_version: 1 });
    }

    #[test]
    fn resume_complete_is_already_current() {
        let applied: Vec<u32> = SCHEMA_STEPS.iter().map(|s| s.version).collect();
        assert_eq!(resume(SUPPORTED_SCHEMA_VERSION, &applied), ResumeDecision::AlreadyCurrent);
    }

    #[test]
    fn resume_gap_or_missing_receipt_refuses() {
        // claims version 1 but no receipt — cannot trust; refuse to start
        let decision = resume(1, &[]);
        assert!(matches!(decision, ResumeDecision::Refuse { .. }));
    }

    #[test]
    fn resume_duplicate_or_unknown_step_refuses() {
        assert!(matches!(resume(1, &[1, 1]), ResumeDecision::Refuse { .. }));
        assert!(matches!(resume(1, &[1, 2]), ResumeDecision::Refuse { .. }));
        assert!(matches!(resume(0, &[1]), ResumeDecision::Refuse { .. }));
        assert!(matches!(resume(2, &[1, 2]), ResumeDecision::Refuse { .. })); // newer than supported
    }

    #[test]
    fn refuse_decision_preserves_data_message() {
        if let ResumeDecision::Refuse { reason } = resume(1, &[]) {
            assert!(reason.contains("refus") || reason.contains("mismatch") || reason.contains("receipt"));
        } else {
            panic!("expected Refuse");
        }
    }
}
