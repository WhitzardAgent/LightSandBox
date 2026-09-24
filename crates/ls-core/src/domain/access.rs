//! Ownership and fencing checks (T04-C01).
//!
//! Check order is a security property: the owner check runs FIRST so a caller
//! probing someone else's sandbox learns FORBIDDEN and nothing about its
//! generation. Only for the rightful owner do we evaluate the incarnation
//! fence (STALE_GENERATION) and deletion (NOT_FOUND). A deleted sandbox id is
//! retired forever — a new incarnation is a new id (docs/v5/03_DOMAIN.md).
//!
//! All predicates are pure: a failed check has no side effect, and a
//! successful CAS returns the NEXT revision for the store to apply atomically.
//! Nothing here mutates the record, so "rejection without journal side
//! effects" holds by construction.

use super::error::{DomainError, ErrorCode};
use super::ids::{ExecutionId, Generation, OwnerId, Revision, SandboxId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxLifetime {
    Active,
    /// Tombstone: the id is retired. There is deliberately NO revive/reuse
    /// operation — deleted ids are never recycled.
    Retired,
}

/// The durable ownership record for one sandbox incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRecord {
    pub id: SandboxId,
    pub owner: OwnerId,
    pub generation: Generation,
    pub revision: Revision,
    pub lifetime: SandboxLifetime,
}

impl SandboxRecord {
    pub fn active(id: SandboxId, owner: OwnerId) -> Self {
        SandboxRecord {
            id,
            owner,
            generation: Generation::INITIAL,
            revision: Revision::INITIAL,
            lifetime: SandboxLifetime::Active,
        }
    }

    pub fn with_incarnation(
        id: SandboxId,
        owner: OwnerId,
        generation: Generation,
        revision: Revision,
    ) -> Self {
        SandboxRecord {
            id,
            owner,
            generation,
            revision,
            lifetime: SandboxLifetime::Active,
        }
    }

    /// The record as retired. Id and generation are preserved strictly so
    /// in-flight references can be fenced with precise errors.
    pub fn retired(self) -> Self {
        SandboxRecord {
            lifetime: SandboxLifetime::Retired,
            ..self
        }
    }

    /// Authorise a read/write against claimed identity.
    ///
    /// * not the owner → FORBIDDEN (even if the generation is also wrong:
    ///   never leak incarnation state to another principal)
    /// * retired id → NOT_FOUND for the owner, FORBIDDEN for anyone else
    /// * claimed generation ≠ record → STALE_GENERATION
    pub fn authorize(
        &self,
        principal: &OwnerId,
        claimed_generation: Option<Generation>,
    ) -> Result<(), DomainError> {
        if principal != &self.owner {
            return Err(DomainError::new(
                ErrorCode::Forbidden,
                "sandbox belongs to another principal",
            ));
        }
        if self.lifetime == SandboxLifetime::Retired {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "sandbox id is retired and never reused",
            ));
        }
        if let Some(claimed) = claimed_generation {
            if claimed != self.generation {
                return Err(DomainError::new(
                    ErrorCode::StaleGeneration,
                    "resource incarnation changed",
                ));
            }
        }
        Ok(())
    }

    /// CAS gate over the record revision. Returns the revision the store must
    /// persist; a rejected check yields nothing and changes nothing.
    ///
    /// Revision conflicts are CONFLICT; incarnation mismatches are
    /// STALE_GENERATION — the two counters are never conflated
    /// (docs/v5/03_DOMAIN.md).
    pub fn cas_write(
        &self,
        principal: &OwnerId,
        claimed_generation: Generation,
        expected_revision: Revision,
    ) -> Result<Revision, DomainError> {
        self.authorize(principal, Some(claimed_generation))?;
        if expected_revision != self.revision {
            return Err(DomainError::new(
                ErrorCode::Conflict,
                "record revision changed; re-read and retry",
            ));
        }
        self.revision.checked_next()
    }
}

/// An execution is bound at start to its sandbox id AND generation; a later
/// incarnation of the sandbox never answers for an old execution id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBinding {
    pub execution_id: ExecutionId,
    pub sandbox_id: SandboxId,
    pub sandbox_generation: Generation,
}

impl ExecutionBinding {
    pub fn new(execution_id: ExecutionId, record: &SandboxRecord) -> Self {
        ExecutionBinding {
            execution_id,
            sandbox_id: record.id.clone(),
            sandbox_generation: record.generation,
        }
    }

    /// Fence this execution against a (possibly newer) sandbox record.
    pub fn authorize_on(&self, record: &SandboxRecord, principal: &OwnerId) -> Result<(), DomainError> {
        if self.sandbox_id != record.id {
            return Err(DomainError::new(
                ErrorCode::NotFound,
                "execution is bound to a different sandbox id",
            ));
        }
        record.authorize(principal, Some(self.sandbox_generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> OwnerId {
        OwnerId::new("alice").unwrap()
    }

    fn bob() -> OwnerId {
        OwnerId::new("bob").unwrap()
    }

    fn alice_record() -> SandboxRecord {
        SandboxRecord::active(SandboxId::new("sb_01").unwrap(), alice())
    }

    // ---- T04-C01: cross-owner or stale generation handles ----

    #[test]
    fn owner_passes_with_no_claimed_generation() {
        assert!(alice_record().authorize(&alice(), None).is_ok());
    }

    #[test]
    fn cross_owner_is_forbidden_even_with_correct_generation() {
        let rec = alice_record();
        let err = rec
            .authorize(&bob(), Some(rec.generation))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn cross_owner_wrong_generation_still_forbidden_not_stale() {
        // Security: never leak incarnation state to another principal.
        let rec = alice_record();
        let err = rec
            .authorize(&bob(), Some(Generation(999)))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn owner_with_stale_generation_is_stale_generation() {
        let rec = alice_record();
        let err = rec.authorize(&alice(), Some(Generation(7))).unwrap_err();
        assert_eq!(err.code, ErrorCode::StaleGeneration);
    }

    #[test]
    fn retired_id_is_not_found_for_owner_and_forbidden_for_others() {
        let rec = alice_record().retired();
        assert_eq!(rec.authorize(&alice(), None).unwrap_err().code, ErrorCode::NotFound);
        assert_eq!(rec.authorize(&bob(), None).unwrap_err().code, ErrorCode::Forbidden);
    }

    #[test]
    fn no_revive_path_exists_after_retire() {
        // Deleted ids are never reused: a new incarnation must be a NEW record
        // with a new id; the old tombstone keeps fencing.
        let rec = alice_record().retired();
        assert!(rec.authorize(&alice(), Some(rec.generation)).is_err());
        let new_incarnation = SandboxRecord::active(SandboxId::new("sb_02").unwrap(), alice());
        assert!(new_incarnation.authorize(&alice(), None).is_ok());
        assert_ne!(rec.id, new_incarnation.id);
    }

    #[test]
    fn cas_success_returns_next_revision_without_mutating() {
        let rec = alice_record();
        let next = rec
            .cas_write(&alice(), rec.generation, rec.revision)
            .unwrap();
        assert_eq!(next, Revision(1));
        assert_eq!(rec.revision, Revision(0)); // &self: no side effect
    }

    #[test]
    fn cas_wrong_revision_is_conflict_and_yields_nothing() {
        let rec = alice_record();
        let err = rec
            .cas_write(&alice(), rec.generation, Revision(5))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(rec.revision, Revision(0));
    }

    #[test]
    fn cas_combines_fences() {
        let rec = alice_record();
        assert_eq!(
            rec.cas_write(&bob(), rec.generation, rec.revision).unwrap_err().code,
            ErrorCode::Forbidden
        );
        assert_eq!(
            rec.cas_write(&alice(), Generation(9), rec.revision).unwrap_err().code,
            ErrorCode::StaleGeneration
        );
    }

    #[test]
    fn execution_binding_fences_old_incarnations() {
        let rec_v1 = SandboxRecord::active(SandboxId::new("sb_01").unwrap(), alice());
        let binding = ExecutionBinding::new(ExecutionId::new("ex_01").unwrap(), &rec_v1);
        assert!(binding.authorize_on(&rec_v1, &alice()).is_ok());

        // A new incarnation (new generation, same id shape — e.g. retry before
        // retirement) never answers for the old execution.
        let rec_v2 = SandboxRecord::with_incarnation(
            SandboxId::new("sb_01").unwrap(),
            alice(),
            Generation(2),
            Revision::INITIAL,
        );
        assert_eq!(
            binding.authorize_on(&rec_v2, &alice()).unwrap_err().code,
            ErrorCode::StaleGeneration
        );

        // And it cannot hop sandbox ids.
        let other = SandboxRecord::active(SandboxId::new("sb_99").unwrap(), alice());
        assert_eq!(
            binding.authorize_on(&other, &alice()).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn execution_binding_cross_owner_forbidden() {
        let rec = alice_record();
        let binding = ExecutionBinding::new(ExecutionId::new("ex_01").unwrap(), &rec);
        assert_eq!(
            binding.authorize_on(&rec, &bob()).unwrap_err().code,
            ErrorCode::Forbidden
        );
    }
}
