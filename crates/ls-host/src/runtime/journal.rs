//! Create/delete journaling with strictly-scoped rollback (docs/v5/08_FAILURES.md).
//!
//! A multi-step bring-up records each completed step's compensation. On any
//! later failure the compensations run newest-first (LIFO), each
//! compensation's OWN error is captured in the journal, and both the
//! original failure and every cleanup error are reported — never swallowed
//! ("同时报告原始错误与清理错误"). On success the journal is committed and
//! NO compensation ever runs.
//!
//! The journal is generic: the manager uses it for image/lease/registry
//! scope, the native backend for bundle/container scope. The resource
//! ownership rule is structural — a step registers exactly the compensation
//! for the resource IT created, so one sandbox's failure can only roll back
//! that sandbox's effects (T08-C03).

use std::fmt;

/// One journal entry, in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEvent {
    /// A named step finished; note is a short human/operator fact.
    StepOk { name: String, note: Option<String> },
    /// A named compensation (rollback of an earlier step) finished.
    Compensated { for_step: String },
    /// A compensation failed; BOTH errors must surface to the caller.
    CompensationFailed { for_step: String, error: String },
}

/// A step's effect plus the (optional) compensation closure that undoes it.
pub struct StepOutcome {
    pub note: Option<String>,
    pub compensate: Option<Box<dyn FnOnce() -> Result<(), String> + Send>>,
}

impl StepOutcome {
    pub fn irreversible(note: impl Into<Option<String>>) -> Self {
        StepOutcome {
            note: note.into(),
            compensate: None,
        }
    }
    pub fn with_compensation<F>(note: impl Into<Option<String>>, f: F) -> Self
    where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        StepOutcome {
            note: note.into(),
            compensate: Some(Box::new(f)),
        }
    }
}

/// The failure that aborted a journaled run.
#[derive(Debug)]
pub struct RolledBack<E> {
    /// The original step failure.
    pub original: E,
    /// Which step failed (the step's own name).
    pub failed_step: String,
    /// Full journal incl. compensation outcomes.
    pub events: Vec<JournalEvent>,
    /// Compensation errors (step, error) — non-empty means the failure
    /// surface is DEGRADED and must be reported as such (cleanup-required).
    pub compensation_errors: Vec<(String, String)>,
}

impl<E: fmt::Display> fmt::Display for RolledBack<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "step {:?} failed: {} (rollback: {} compensated, {} compensation errors)",
            self.failed_step,
            self.original,
            self.events
                .iter()
                .filter(|e| matches!(e, JournalEvent::Compensated { .. }))
                .count(),
            self.compensation_errors.len()
        )
    }
}

/// An in-flight journaled run. Single-owner, sequential — concurrency
/// fencing lives in the manager registry, not here.
pub struct Journal {
    events: Vec<JournalEvent>,
    compensations: Vec<(String, Box<dyn FnOnce() -> Result<(), String> + Send>)>,
}

impl Journal {
    pub fn new() -> Self {
        Journal {
            events: Vec::new(),
            compensations: Vec::new(),
        }
    }

    pub fn events(&self) -> &[JournalEvent] {
        &self.events
    }

    /// Run one named step: on success record it and stash its compensation;
    /// on failure roll back every earlier step (LIFO) and return the full
    /// failure surface. The step's own error is passed through unchanged.
    pub fn step<E>(
        &mut self,
        name: impl Into<String>,
        effect: impl FnOnce() -> Result<StepOutcome, E>,
    ) -> Result<(), RolledBack<E>> {
        self.step_value(name, || effect().map(|o| ((), o))).map(|_| ())
    }

    /// Same as [`Journal::step`] but the step also yields a value (created
    /// resource handle etc) for subsequent steps and the success path.
    pub fn step_value<T, E>(
        &mut self,
        name: impl Into<String>,
        effect: impl FnOnce() -> Result<(T, StepOutcome), E>,
    ) -> Result<T, RolledBack<E>> {
        let name = name.into();
        match effect() {
            Ok((value, outcome)) => {
                if let Some(comp) = outcome.compensate {
                    self.compensations.push((name.clone(), comp));
                }
                self.events.push(JournalEvent::StepOk {
                    name,
                    note: outcome.note,
                });
                Ok(value)
            }
            Err(original) => {
                let compensation_errors =
                    Self::run_compensations(&mut self.compensations, &mut self.events);
                Err(RolledBack {
                    original,
                    failed_step: name,
                    events: std::mem::take(&mut self.events),
                    compensation_errors,
                })
            }
        }
    }

    /// Abort now with `original`, rolling back all recorded steps.
    pub fn fail_and_rollback<E>(mut self, failed_step: String, original: E) -> RolledBack<E> {
        let compensation_errors =
            Self::run_compensations(&mut self.compensations, &mut self.events);
        RolledBack {
            original,
            failed_step,
            events: self.events,
            compensation_errors,
        }
    }

    /// LIFO compensation pass, newest first. Every failure is retained in
    /// BOTH the event log and the returned (step, error) list — a degraded
    /// rollback is always visible, never silently cleaner than it was.
    fn run_compensations(
        compensations: &mut Vec<(String, Box<dyn FnOnce() -> Result<(), String> + Send>)>,
        events: &mut Vec<JournalEvent>,
    ) -> Vec<(String, String)> {
        let mut compensation_errors = Vec::new();
        while let Some((step_name, comp)) = compensations.pop() {
            match comp() {
                Ok(()) => events.push(JournalEvent::Compensated {
                    for_step: step_name,
                }),
                Err(error) => {
                    compensation_errors.push((step_name.clone(), error.clone()));
                    events.push(JournalEvent::CompensationFailed {
                        for_step: step_name,
                        error,
                    });
                }
            }
        }
        compensation_errors
    }

    /// Success closure: no compensation ever runs after commit.
    pub fn commit(self) -> Vec<JournalEvent> {
        self.events
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn recorded() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[test]
    fn happy_path_commits_without_compensating() {
        let log = recorded();
        let mut j = Journal::new();
        {
            let log = log.clone();
            j.step::<String>("lease", || {
                log.lock().unwrap().push("effect-lease".into());
                Ok(StepOutcome::with_compensation(None, {
                    let log = log.clone();
                    move || {
                        log.lock().unwrap().push("undo-lease".into());
                        Ok(())
                    }
                }))
            })
            .unwrap();
        }
        j.step::<String>("bundle", || {
            Ok(StepOutcome::irreversible(Some("bundle written".into())))
        })
        .unwrap();
        let events = j.commit();
        assert_eq!(events.len(), 2);
        assert!(log.lock().unwrap().iter().all(|s| s != "undo-lease"));
    }

    #[test]
    fn failure_rolls_back_strictly_lifo_and_preserves_original() {
        let log = recorded();
        let mut j = Journal::new();
        for name in ["a", "b", "c"] {
            let log = log.clone();
            j.step::<String>(name, || {
                Ok(StepOutcome::with_compensation(None, {
                    let log = log.clone();
                    move || {
                        log.lock().unwrap().push(format!("undo-{name}"));
                        Ok(())
                    }
                }))
            })
            .unwrap();
        }
        let rb = j
            .step("d", || Err::<StepOutcome, String>("boom-d".into()))
            .unwrap_err();
        assert_eq!(rb.original, "boom-d");
        assert_eq!(rb.failed_step, "d");
        assert_eq!(rb.compensation_errors.len(), 0);
        assert_eq!(
            log.lock().unwrap().as_slice(),
            &["undo-c".to_string(), "undo-b".to_string(), "undo-a".to_string()],
            "compensations must run newest-first"
        );
        // journal records compensation for a,b,c in that order
        let compensated: Vec<_> = rb
            .events
            .iter()
            .filter_map(|e| match e {
                JournalEvent::Compensated { for_step } => Some(for_step.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(compensated, vec!["c", "b", "a"]);
    }

    #[test]
    fn compensation_error_is_reported_alongside_original() {
        let mut j = Journal::new();
        j.step::<String>("container", || {
            Ok(StepOutcome::with_compensation(None, || {
                Err("runc delete: EBUSY".to_string())
            }))
        })
        .unwrap();
        let rb = j
            .fail_and_rollback("start".to_string(), "start failed real".to_string())
            ;
        assert_eq!(rb.original, "start failed real");
        assert_eq!(
            rb.compensation_errors,
            vec![("container".to_string(), "runc delete: EBUSY".to_string())]
        );
        let shown = rb.to_string();
        assert!(shown.contains("start failed real"));
        assert!(shown.contains("1 compensation errors"));
        assert!(rb
            .events
            .iter()
            .any(|e| matches!(e, JournalEvent::CompensationFailed { for_step, .. } if for_step == "container")));
    }

    #[test]
    fn committed_journal_never_rolls_back() {
        let log = recorded();
        let mut j = Journal::new();
        {
            let log = log.clone();
            j.step::<String>("x", || {
                Ok(StepOutcome::with_compensation(None, move || {
                    log.lock().unwrap().push("undo-x".into());
                    Ok(())
                }))
            })
            .unwrap();
        }
        let _ = j.commit();
        assert!(log.lock().unwrap().is_empty());
    }
}
