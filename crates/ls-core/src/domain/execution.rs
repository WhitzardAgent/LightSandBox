//! Executions: one durable identity per command inside a sandbox
//! (docs/v5/03_DOMAIN.md; T04).
//!
//! Hard rules pinned by tests:
//! * accepted → starting → running → exited (+ start-failed / unknown), per
//!   the frozen table in lifecycle.rs
//! * state `exited` REQUIRES a real observed exit record (wait status /
//!   trusted supervisor receipt). An exit code — even 137 — never touches the
//!   parent management Operation
//! * timeout / cancel / OOM are an INDEPENDENT termination-reason dimension
//!   with observation evidence, not a state hack
//! * `unknown` re-confirms only via an audited authoritative receipt,
//!   never by re-execution
//!
//! Side-effect discipline: legality is ALWAYS proven before any field moves,
//! so a rejected call leaves the execution bit-identical.

use super::access::ExecutionBinding;
use super::error::{DomainError, ErrorCode};
use super::ids::ExecutionId;
use super::lifecycle::ExecutionState;

/// Mutually exclusive exit record (docs/v5/03_DOMAIN.md §命令与退出).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRecord {
    Exited { exit_code: i32 },
    Signaled { signal: u32 },
    /// The infrastructure lost the result: allowed only alongside state
    /// `unknown`; never a silent success.
    Unknown,
}

impl ExitRecord {
    /// A real observed receipt (not the unknown placeholder). Required for
    /// state `exited`.
    fn is_observed(self) -> bool {
        matches!(self, ExitRecord::Exited { .. } | ExitRecord::Signaled { .. })
    }
}

/// Independent termination dimension: WHY the execution was torn down,
/// orthogonal to the observed exit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    None,
    Timeout,
    Cancelled,
    OomKilled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    id: ExecutionId,
    binding: ExecutionBinding,
    state: ExecutionState,
    exit_record: Option<ExitRecord>,
    termination: TerminationReason,
    audits: Vec<String>,
}

impl Execution {
    pub fn new(id: ExecutionId, binding: ExecutionBinding) -> Self {
        Execution {
            id,
            binding,
            state: ExecutionState::Accepted,
            exit_record: None,
            termination: TerminationReason::None,
            audits: Vec::new(),
        }
    }

    pub fn id(&self) -> &ExecutionId {
        &self.id
    }

    pub fn binding(&self) -> &ExecutionBinding {
        &self.binding
    }

    pub fn state(&self) -> ExecutionState {
        self.state
    }

    pub fn exit_record(&self) -> Option<ExitRecord> {
        self.exit_record
    }

    pub fn termination(&self) -> TerminationReason {
        self.termination
    }

    pub fn audits(&self) -> &[String] {
        &self.audits
    }

    pub fn transition(&mut self, to: ExecutionState) -> Result<(), DomainError> {
        // table legality first — no field may move on an illegal edge
        let next = self.state.transition(to)?;
        // `exited` is additionally evidence-gated: only finish_* (which
        // installs a real record) may enter it.
        if to == ExecutionState::Exited && !self.exit_record.is_some_and(ExitRecord::is_observed) {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "state exited requires an observed exit record (use finish_exited/finish_signaled)",
            ));
        }
        self.state = next;
        Ok(())
    }

    /// Record the observed wait status and move to exited. The ONLY way to
    /// enter `exited` (semantic fact: exited requires a record).
    pub fn finish_exited(&mut self, exit_code: i32) -> Result<(), DomainError> {
        let next = self.state.transition(ExecutionState::Exited)?;
        self.exit_record = Some(ExitRecord::Exited { exit_code });
        self.state = next;
        Ok(())
    }

    pub fn finish_signaled(&mut self, signal: u32) -> Result<(), DomainError> {
        let next = self.state.transition(ExecutionState::Exited)?;
        self.exit_record = Some(ExitRecord::Signaled { signal });
        self.state = next;
        Ok(())
    }

    /// The result channel was lost: mark unknown with an unknown record.
    /// Terminal, but escapable via `reconcile_unknown` with a real receipt.
    pub fn finish_unknown(&mut self) -> Result<(), DomainError> {
        let next = self.state.transition(ExecutionState::Unknown)?;
        self.exit_record = Some(ExitRecord::Unknown);
        self.state = next;
        Ok(())
    }

    /// Start never produced a guest process; no ghost execution, no record.
    pub fn fail_start(&mut self) -> Result<(), DomainError> {
        self.state = self.state.transition(ExecutionState::StartFailed)?;
        Ok(())
    }

    /// Record an OVERRIDDEN termination reason. Set-once; a repeat of the
    /// SAME reason is idempotent even after termination (it carries no new
    /// information), but a NEW reason on a terminal execution is CONFLICT —
    /// "cancel accepted is not cancel completed" (03_DOMAIN).
    pub fn mark_termination(&mut self, reason: TerminationReason) -> Result<(), DomainError> {
        if reason == TerminationReason::None {
            return Err(DomainError::invalid("termination reason none is not a markable event"));
        }
        match self.termination {
            existing if existing == reason => Ok(()), // idempotent repeat
            TerminationReason::None => {
                if self.state.is_terminal() {
                    return Err(DomainError::new(
                        ErrorCode::Conflict,
                        "cancel/timeout/OOM of an already-terminal execution conflicts; it is not a completed cancel",
                    ));
                }
                self.termination = reason;
                Ok(())
            }
            existing => Err(DomainError::new(
                ErrorCode::Conflict,
                format!("termination already recorded as {existing:?}"),
            )),
        }
    }

    /// The ONLY escape from `unknown`: an authoritative exit/start-failure
    /// receipt plus mandatory audit. Old unknown record is never erased —
    /// the audit trail preserves that it WAS unknown.
    pub fn reconcile_unknown(
        &mut self,
        receipt: ExitReceipt,
        audit: impl Into<String>,
    ) -> Result<(), DomainError> {
        if self.state != ExecutionState::Unknown {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "only an unknown execution reconciles; everything else follows the frozen table",
            ));
        }
        let audit: String = audit.into();
        if audit.is_empty() {
            return Err(DomainError::invalid(
                "reconcile requires a non-empty audit note naming the authoritative receipt",
            ));
        }
        let (record, target) = match receipt {
            ExitReceipt::Exited { exit_code } => {
                (Some(ExitRecord::Exited { exit_code }), ExecutionState::Exited)
            }
            ExitReceipt::Signaled { signal } => {
                (Some(ExitRecord::Signaled { signal }), ExecutionState::Exited)
            }
            ExitReceipt::StartFailed => (None, ExecutionState::StartFailed),
        };
        self.audits.push(format!(
            "unknown execution reconciled to {}: {}",
            target.wire(),
            audit
        ));
        self.exit_record = record;
        self.state = target;
        Ok(())
    }
}

/// What a late authoritative receipt may claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReceipt {
    Exited { exit_code: i32 },
    Signaled { signal: u32 },
    StartFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::fixture_json;
    use crate::domain::access::SandboxRecord;
    use crate::domain::ids::{ExecutionId, OwnerId, SandboxId};

    fn exec() -> Execution {
        let rec = SandboxRecord::active(SandboxId::new("sb_01").unwrap(), OwnerId::new("alice").unwrap());
        let binding = ExecutionBinding::new(ExecutionId::new("ex_01").unwrap(), &rec);
        Execution::new(ExecutionId::new("ex_01").unwrap(), binding)
    }

    fn running_exec() -> Execution {
        let mut e = exec();
        e.transition(ExecutionState::Starting).unwrap();
        e.transition(ExecutionState::Running).unwrap();
        e
    }

    // ---- the 137 semantic lock (Operation ≠ Execution) ----

    #[test]
    fn exit_137_is_exited_with_code_not_failure() {
        let mut e = running_exec();
        e.finish_exited(137).unwrap();
        assert_eq!(e.state(), ExecutionState::Exited);
        assert_eq!(e.exit_record(), Some(ExitRecord::Exited { exit_code: 137 }));
        assert_eq!(e.termination(), TerminationReason::None);
    }

    #[test]
    fn signaled_records_signal() {
        let mut e = running_exec();
        e.finish_signaled(9).unwrap();
        assert_eq!(e.state(), ExecutionState::Exited);
        assert_eq!(e.exit_record(), Some(ExitRecord::Signaled { signal: 9 }));
    }

    #[test]
    fn exited_without_record_is_impossible_by_construction() {
        let mut e = running_exec();
        // direct transition into Exited without a record is rejected
        assert!(e.transition(ExecutionState::Exited).is_err());
        assert_eq!(e.state(), ExecutionState::Running);
    }

    #[test]
    fn failed_finish_from_illegal_state_has_no_side_effect() {
        // Starting -> Exited is not a frozen edge: the record must NOT move.
        let mut e = exec();
        e.transition(ExecutionState::Starting).unwrap();
        assert!(e.finish_exited(0).is_err());
        assert_eq!(e.exit_record(), None);
        assert_eq!(e.state(), ExecutionState::Starting);
    }

    #[test]
    fn start_failed_has_no_ghost_record() {
        let mut e = exec();
        e.transition(ExecutionState::Starting).unwrap();
        e.fail_start().unwrap();
        assert_eq!(e.state(), ExecutionState::StartFailed);
        assert_eq!(e.exit_record(), None);
    }

    // ---- fixture: illegal transitions & semantic facts ----

    #[test]
    fn illegal_transitions_from_frozen_fixture_hold() {
        let cases = fixture_json("fixtures/states/cases.json");
        for case in cases["illegal_transitions"].as_array().unwrap() {
            if case["domain"].as_str().unwrap() != "execution" {
                continue;
            }
            let from = case["from"].as_str().unwrap();
            let to = case["to"].as_str().unwrap();
            let mut e = driven_to(from);
            let target = match to {
                "running" => ExecutionState::Running,
                "exited" => ExecutionState::Exited,
                other => panic!("unexpected fixture target {other}"),
            };
            assert!(e.transition(target).is_err(), "execution {from} -> {to}");
        }
    }

    fn driven_to(name: &str) -> Execution {
        let mut e = exec();
        match name {
            "accepted" => {}
            "running" => {
                e = running_exec();
            }
            "exited" => {
                e = running_exec();
                e.finish_exited(0).unwrap();
            }
            "start-failed" => {
                e.transition(ExecutionState::Starting).unwrap();
                e.fail_start().unwrap();
            }
            "unknown" => {
                e = running_exec();
                e.finish_unknown().unwrap();
            }
            other => panic!("no driver for {other}"),
        }
        e
    }

    #[test]
    fn semantic_facts_fixture_matches_domain_consistency() {
        let cases = fixture_json("fixtures/states/cases.json");
        for fact in cases["semantic_facts"].as_array().unwrap() {
            let name = fact["name"].as_str().unwrap();
            let expect = fact["consistent"].as_bool().unwrap();
            let state = fact["document"]["execution"]["state"].as_str().unwrap();
            let record = &fact["document"]["execution"]["exit_record"];
            let consistent = !(state == "exited" && record.is_null());
            assert_eq!(consistent, expect, "semantic fact {name}");
        }
    }

    // ---- unknown & reconcile ----

    #[test]
    fn unknown_sets_unknown_record_and_is_terminal() {
        let mut e = running_exec();
        e.finish_unknown().unwrap();
        assert_eq!(e.state(), ExecutionState::Unknown);
        assert_eq!(e.exit_record(), Some(ExitRecord::Unknown));
        assert!(e.transition(ExecutionState::Running).is_err());
    }

    #[test]
    fn unknown_reconciles_with_authoritative_receipt_and_audit() {
        let mut e = running_exec();
        e.finish_unknown().unwrap();
        e.reconcile_unknown(ExitReceipt::Exited { exit_code: 3 }, "supervisor receipt sr-2")
            .unwrap();
        assert_eq!(e.state(), ExecutionState::Exited);
        assert_eq!(e.exit_record(), Some(ExitRecord::Exited { exit_code: 3 }));
        assert_eq!(e.audits().len(), 1);
    }

    #[test]
    fn unknown_reconcile_start_failed_receipt() {
        let mut e = exec();
        e.transition(ExecutionState::Starting).unwrap();
        e.finish_unknown().unwrap();
        e.reconcile_unknown(ExitReceipt::StartFailed, "runc create receipt").unwrap();
        assert_eq!(e.state(), ExecutionState::StartFailed);
        assert_eq!(e.exit_record(), None);
    }

    #[test]
    fn unknown_reconcile_requires_non_empty_audit() {
        let mut e = running_exec();
        e.finish_unknown().unwrap();
        let err = e.reconcile_unknown(ExitReceipt::Exited { exit_code: 0 }, "").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(e.state(), ExecutionState::Unknown);
    }

    #[test]
    fn reconcile_on_non_unknown_is_conflict() {
        let mut e = running_exec();
        let err = e
            .reconcile_unknown(ExitReceipt::Exited { exit_code: 0 }, "out of band")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }

    // ---- independent termination dimension ----

    #[test]
    fn termination_reason_is_independent_and_set_once() {
        let mut e = running_exec();
        e.mark_termination(TerminationReason::Timeout).unwrap();
        assert_eq!(e.termination(), TerminationReason::Timeout);
        // a late observed exit record still fits the same execution
        e.finish_exited(124).unwrap();
        assert_eq!(e.state(), ExecutionState::Exited);
        assert_eq!(e.exit_record(), Some(ExitRecord::Exited { exit_code: 124 }));
        assert_eq!(e.termination(), TerminationReason::Timeout);
        // second different reason conflicts
        assert_eq!(
            e.mark_termination(TerminationReason::Cancelled).unwrap_err().code,
            ErrorCode::Conflict
        );
        // idempotent repeat of the SAME reason is fine even after exit
        e.mark_termination(TerminationReason::Timeout).unwrap();
    }

    #[test]
    fn cancel_of_terminal_execution_conflicts() {
        let mut e = running_exec();
        e.finish_exited(0).unwrap();
        assert_eq!(
            e.mark_termination(TerminationReason::Cancelled).unwrap_err().code,
            ErrorCode::Conflict
        );
    }
}
