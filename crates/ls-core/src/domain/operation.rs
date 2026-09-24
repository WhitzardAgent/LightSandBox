//! Management operations: create / delete / prepare / snapshot
//! (docs/v5/03_DOMAIN.md §命令与退出; T04).
//!
//! Hard rules pinned by tests:
//! * pending → running → succeeded|failed|unknown (frozen table, lifecycle.rs)
//! * `unknown` NEVER re-executes: no transition leaves it except the audited
//!   [`Operation::reconcile_unknown`] path with an authoritative receipt
//! * create/prepare/snapshot success MUST carry a real SandboxRef/TemplateRef;
//!   delete success MUST carry a CleanupRef. Success without its reference is
//!   an INTERNAL_FAILURE, not a silent pass.

use super::error::{DomainError, ErrorCode};
use super::ids::{BundleRef, CleanupId, Generation, OperationId, SandboxId, TemplateId};
use super::lifecycle::OperationState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Create,
    Delete,
    Prepare,
    Snapshot,
}

impl OperationKind {
    pub fn wire(self) -> &'static str {
        match self {
            OperationKind::Create => "create",
            OperationKind::Delete => "delete",
            OperationKind::Prepare => "prepare",
            OperationKind::Snapshot => "snapshot",
        }
    }
}

/// A handle an operation success must hand back. Deletion hands the cleanup
/// record; creation/preparation/snapshot hand the minted object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationResult {
    Sandbox { sandbox: SandboxId, generation: Generation },
    Template { template: TemplateId, image_digest: BundleRef },
    Cleanup { cleanup: CleanupId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    id: OperationId,
    kind: OperationKind,
    state: OperationState,
    result: Option<OperationResult>,
    audits: Vec<String>,
}

impl Operation {
    pub fn new(id: OperationId, kind: OperationKind) -> Self {
        Operation {
            id,
            kind,
            state: OperationState::Pending,
            result: None,
            audits: Vec::new(),
        }
    }

    pub fn id(&self) -> &OperationId {
        &self.id
    }

    pub fn kind(&self) -> OperationKind {
        self.kind
    }

    pub fn state(&self) -> OperationState {
        self.state
    }

    pub fn audits(&self) -> &[String] {
        &self.audits
    }

    /// Attach the outcome reference while the operation runs. The success
    /// transition validates kind-compatibility via `validate_success`.
    pub fn attach_result(&mut self, result: OperationResult) -> Result<(), DomainError> {
        if self.state != OperationState::Running {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "operation results attach only while running",
            ));
        }
        self.result = Some(result);
        Ok(())
    }

    /// One legal step per the frozen table. Terminal states have no outgoing
    /// edges; unknown is only escapable via `reconcile_unknown`.
    pub fn transition(&mut self, to: OperationState) -> Result<(), DomainError> {
        // success is evidence-gated BEFORE the state moves (no partial apply)
        if to == OperationState::Succeeded {
            self.validate_success()?;
        }
        self.state = self.state.transition(to)?;
        Ok(())
    }

    /// Success is only legal WITH a kind-matching result reference.
    fn validate_success(&self) -> Result<(), DomainError> {
        let result = self.result.as_ref().ok_or_else(|| {
            DomainError::new(
                ErrorCode::InternalFailure,
                format!("{} success requires its result reference", self.kind.wire()),
            )
        })?;
        let compatible = matches!(
            (self.kind, result),
            (OperationKind::Create, OperationResult::Sandbox { .. })
                | (OperationKind::Prepare, OperationResult::Template { .. })
                | (OperationKind::Snapshot, OperationResult::Template { .. })
                | (OperationKind::Delete, OperationResult::Cleanup { .. })
        );
        if !compatible {
            return Err(DomainError::new(
                ErrorCode::InternalFailure,
                format!(
                    "{} success carries an incompatible result reference",
                    self.kind.wire()
                ),
            ));
        }
        Ok(())
    }

    /// The ONLY way an unknown operation may change state: an authoritative
    /// receipt arrives later, moving it to a CONFIRMED terminal state with a
    /// mandatory audit note. The original unknown record is never erased and
    /// the operation is never re-executed.
    pub fn reconcile_unknown(
        &mut self,
        confirmed: ReconciledOutcome,
        audit: impl Into<String>,
    ) -> Result<(), DomainError> {
        if self.state != OperationState::Unknown {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "only an unknown operation reconciles; everything else follows the frozen table",
            ));
        }
        let audit: String = audit.into();
        if audit.is_empty() {
            return Err(DomainError::invalid(
                "reconcile requires a non-empty audit note naming the authoritative receipt",
            ));
        }
        let target = match confirmed {
            ReconciledOutcome::Succeeded => {
                // late success still must carry its result reference
                self.validate_success()?;
                OperationState::Succeeded
            }
            ReconciledOutcome::Failed => OperationState::Failed,
        };
        self.audits
            .push(format!("unknown reconciled to {}: {}", target.wire(), audit));
        self.state = target;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciledOutcome {
    Succeeded,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::fixture_json;

    fn op(kind: OperationKind) -> Operation {
        Operation::new(OperationId::new("op_01").unwrap(), kind)
    }

    fn sandbox_result() -> OperationResult {
        OperationResult::Sandbox {
            sandbox: SandboxId::new("sb_01").unwrap(),
            generation: Generation::INITIAL,
        }
    }

    fn cleanup_result() -> OperationResult {
        OperationResult::Cleanup {
            cleanup: CleanupId::new("cl_01").unwrap(),
        }
    }

    #[test]
    fn happy_path_pending_running_succeeded_with_ref() {
        let mut o = op(OperationKind::Create);
        o.transition(OperationState::Running).unwrap();
        o.attach_result(sandbox_result()).unwrap();
        o.transition(OperationState::Succeeded).unwrap();
        assert_eq!(o.state(), OperationState::Succeeded);
    }

    #[test]
    fn create_succeeded_without_result_is_internal_failure() {
        let mut o = op(OperationKind::Create);
        o.transition(OperationState::Running).unwrap();
        let err = o.transition(OperationState::Succeeded).unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalFailure);
        assert_eq!(o.state(), OperationState::Running); // unchanged
    }

    #[test]
    fn delete_succeeded_requires_cleanup_ref_not_sandbox_ref() {
        let mut o = op(OperationKind::Delete);
        o.transition(OperationState::Running).unwrap();
        o.attach_result(sandbox_result()).unwrap(); // wrong kind of ref
        assert_eq!(
            o.transition(OperationState::Succeeded).unwrap_err().code,
            ErrorCode::InternalFailure
        );

        let mut o2 = op(OperationKind::Delete);
        o2.transition(OperationState::Running).unwrap();
        o2.attach_result(cleanup_result()).unwrap();
        o2.transition(OperationState::Succeeded).unwrap();
        assert_eq!(o2.state(), OperationState::Succeeded);
    }

    #[test]
    fn attach_result_only_while_running() {
        let mut o = op(OperationKind::Create);
        assert!(o.attach_result(sandbox_result()).is_err());
    }

    #[test]
    fn illegal_transitions_from_frozen_fixture_hold() {
        let cases = fixture_json("fixtures/states/cases.json");
        for case in cases["illegal_transitions"].as_array().unwrap() {
            if case["domain"].as_str().unwrap() != "operation" {
                continue;
            }
            let from = case["from"].as_str().unwrap();
            let to = case["to"].as_str().unwrap();
            let mut o = driven_to(from);
            let target = match to {
                "running" => OperationState::Running,
                "succeeded" => OperationState::Succeeded,
                other => panic!("unexpected fixture target {other}"),
            };
            assert!(o.transition(target).is_err(), "operation {from} -> {to}");
        }
    }

    /// Drive an operation into the named state through LEGAL edges (attaching
    /// valid results where success is needed — we only test terminal states
    /// here, from which every edge is illegal anyway).
    fn driven_to(name: &str) -> Operation {
        let mut o = op(OperationKind::Create);
        match name {
            "pending" => {}
            "running" => {
                o.transition(OperationState::Running).unwrap();
            }
            "succeeded" => {
                o.transition(OperationState::Running).unwrap();
                o.attach_result(sandbox_result()).unwrap();
                o.transition(OperationState::Succeeded).unwrap();
            }
            "failed" => {
                o.transition(OperationState::Running).unwrap();
                o.transition(OperationState::Failed).unwrap();
            }
            "unknown" => {
                o.transition(OperationState::Running).unwrap();
                o.transition(OperationState::Unknown).unwrap();
            }
            other => panic!("no driver for {other}"),
        }
        o
    }

    // ---- late confirmation rules (contracts/v1/fixtures/states/cases.json) ----

    #[test]
    fn unknown_reconciles_to_confirmed_terminal_with_audit() {
        let mut o = op(OperationKind::Create);
        o.transition(OperationState::Running).unwrap();
        o.attach_result(sandbox_result()).unwrap();
        o.transition(OperationState::Unknown).unwrap();
        o.reconcile_unknown(ReconciledOutcome::Succeeded, "authoritative receipt rc-9")
            .unwrap();
        assert_eq!(o.state(), OperationState::Succeeded);
        assert_eq!(o.audits().len(), 1);
        assert!(o.audits()[0].contains("rc-9"));
    }

    #[test]
    fn unknown_reconcile_requires_non_empty_audit() {
        let mut o = driven_to("unknown");
        let err = o.reconcile_unknown(ReconciledOutcome::Failed, "").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(o.state(), OperationState::Unknown); // unchanged
    }

    #[test]
    fn unknown_reconcile_succeeded_still_requires_result_ref() {
        let mut o = driven_to("unknown"); // no result attached
        let err = o
            .reconcile_unknown(ReconciledOutcome::Succeeded, "receipt with claimed success")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalFailure);
        assert_eq!(o.state(), OperationState::Unknown);
    }

    #[test]
    fn unknown_cannot_return_to_running_or_pending_by_any_path() {
        let mut o = driven_to("unknown");
        assert!(o.transition(OperationState::Running).is_err());
        assert!(o.transition(OperationState::Pending).is_err());
        // reconcile offers no path back either: it takes only terminal outcomes
        // (compile-time guarantee — ReconciledOutcome has no Running variant).
    }

    #[test]
    fn reconcile_on_non_unknown_is_conflict() {
        let mut o = driven_to("running");
        let err = o
            .reconcile_unknown(ReconciledOutcome::Failed, "out of band")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
    }
}
