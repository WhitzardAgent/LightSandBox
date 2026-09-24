//! Lifecycle state machines for Operation / Execution / Sandbox.
//!
//! The adjacency tables here are locked bidirectionally against the frozen
//! machine-readable source `contracts/v1/states.json` (T41): the lock test
//! proves every legal-and-only-legal transition agrees with the contract.
//! Late confirmation (unknown → terminal receipt) is deliberately NOT part of
//! the base table; it is an audited reconcile in operation.rs / execution.rs.

use super::error::DomainError;
use serde::{Deserialize, Serialize};

macro_rules! state_machine {
    ($name:ident, [ $(($variant:ident, $wire:literal)),+ $(,)? ], [ $(($from:ident => [$($to:ident),*])),* $(,)? ]) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name {
            $( $variant ),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[ $( $name::$variant ),+ ];

            pub fn wire(self) -> &'static str {
                match self {
                    $( $name::$variant => $wire ),+
                }
            }

            /// Legal one-step transition per the frozen contract table.
            pub fn can_transition(self, to: $name) -> bool {
                let targets: &[$name] = match self {
                    $( $name::$from => &[ $( $name::$to ),* ], )*
                };
                targets.contains(&to)
            }

            pub fn is_terminal(self) -> bool {
                !Self::ALL.iter().any(|&t| self.can_transition(t))
            }

            pub fn transition(self, to: $name) -> Result<$name, DomainError> {
                if self.can_transition(to) {
                    Ok(to)
                } else {
                    Err(DomainError::new(
                        super::error::ErrorCode::Conflict,
                        format!("illegal {} transition {} -> {}", stringify!($name), self.wire(), to.wire()),
                    ))
                }
            }
        }
    };
}

state_machine!(
    OperationState,
    [
        (Pending, "pending"),
        (Running, "running"),
        (Succeeded, "succeeded"),
        (Failed, "failed"),
        (Unknown, "unknown"),
    ],
    [
        (Pending => [Running, Failed]),
        (Running => [Succeeded, Failed, Unknown]),
        (Succeeded => []),
        (Failed => []),
        (Unknown => []),
    ]
);

state_machine!(
    ExecutionState,
    [
        (Accepted, "accepted"),
        (Starting, "starting"),
        (Running, "running"),
        (Exited, "exited"),
        (StartFailed, "start-failed"),
        (Unknown, "unknown"),
    ],
    [
        (Accepted => [Starting, StartFailed]),
        (Starting => [Running, StartFailed, Unknown]),
        (Running => [Exited, Unknown]),
        (Exited => []),
        (StartFailed => []),
        (Unknown => []),
    ]
);

state_machine!(
    SandboxState,
    [
        (Requested, "requested"),
        (Preparing, "preparing"),
        (Created, "created"),
        (Starting, "starting"),
        (Running, "running"),
        (Pausing, "pausing"),
        (Paused, "paused"),
        (Resuming, "resuming"),
        (Stopping, "stopping"),
        (Stopped, "stopped"),
        (Failed, "failed"),
        (Deleting, "deleting"),
        (CleanupRequired, "cleanup-required"),
        (Deleted, "deleted"),
    ],
    [
        (Requested => [Preparing, Failed]),
        (Preparing => [Created, Failed]),
        (Created => [Starting, Deleting]),
        (Starting => [Running, Stopped, Failed]),
        (Running => [Pausing, Stopping, Stopped]),
        (Pausing => [Paused, Failed]),
        (Paused => [Resuming, Stopping]),
        (Resuming => [Running, Failed]),
        (Stopping => [Stopped, CleanupRequired]),
        (Stopped => [Deleting]),
        (Failed => [Deleting]),
        (Deleting => [Deleted, CleanupRequired]),
        (CleanupRequired => [Deleting]),
        (Deleted => []),
    ]
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::fixture_json;
    use std::collections::BTreeMap;

    fn contract_table(domain: &str) -> BTreeMap<String, Vec<String>> {
        let states = fixture_json("states.json");
        let raw = states["domains"][domain].as_object().expect("domain table");
        raw.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect(),
                )
            })
            .collect()
    }

    fn assert_table_locked<S: Copy>(all: &[S], wire: impl Fn(S) -> &'static str, can: impl Fn(S, S) -> bool, domain: &str) {
        let table = contract_table(domain);
        let mut ours = BTreeMap::new();
        for &from in all {
            ours.insert(
                wire(from).to_string(),
                all.iter().filter(|&&to| can(from, to)).map(|&t| wire(t).to_string()).collect::<Vec<_>>(),
            );
        }
        let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (from, mut tos) in table {
            tos.sort();
            expected.insert(from, tos);
        }
        let mut ours_sorted = BTreeMap::new();
        for (from, mut tos) in ours {
            tos.sort();
            ours_sorted.insert(from, tos);
        }
        assert_eq!(ours_sorted, expected, "{domain} table drifted from contracts/v1/states.json");
    }

    #[test]
    fn operation_table_matches_frozen_contract() {
        assert_table_locked(OperationState::ALL, |s| s.wire(), |a, b| a.can_transition(b), "operation");
    }

    #[test]
    fn execution_table_matches_frozen_contract() {
        assert_table_locked(ExecutionState::ALL, |s| s.wire(), |a, b| a.can_transition(b), "execution");
    }

    #[test]
    fn sandbox_table_matches_frozen_contract() {
        assert_table_locked(SandboxState::ALL, |s| s.wire(), |a, b| a.can_transition(b), "sandbox");
    }

    fn state_by_wire<S: Copy>(all: &[S], wire: impl Fn(S) -> &'static str, name: &str) -> S {
        *all.iter().find(|&&s| wire(s) == name).unwrap_or_else(|| panic!("no state {name}"))
    }

    #[test]
    fn illegal_transitions_fixture_holds_typed() {
        let cases = fixture_json("fixtures/states/cases.json");
        for case in cases["illegal_transitions"].as_array().unwrap() {
            let domain = case["domain"].as_str().unwrap();
            let from = case["from"].as_str().unwrap();
            let to = case["to"].as_str().unwrap();
            let rejected = match domain {
                "operation" => {
                    let f = state_by_wire(OperationState::ALL, |s| s.wire(), from);
                    let t = state_by_wire(OperationState::ALL, |s| s.wire(), to);
                    f.transition(t).is_err()
                }
                "execution" => {
                    let f = state_by_wire(ExecutionState::ALL, |s| s.wire(), from);
                    let t = state_by_wire(ExecutionState::ALL, |s| s.wire(), to);
                    f.transition(t).is_err()
                }
                "sandbox" => {
                    let f = state_by_wire(SandboxState::ALL, |s| s.wire(), from);
                    let t = state_by_wire(SandboxState::ALL, |s| s.wire(), to);
                    f.transition(t).is_err()
                }
                other => panic!("unknown domain {other}"),
            };
            assert!(rejected, "{domain}: {from} -> {to} must be rejected");
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_edges() {
        assert!(OperationState::Succeeded.is_terminal());
        assert!(OperationState::Failed.is_terminal());
        assert!(OperationState::Unknown.is_terminal());
        assert!(ExecutionState::Exited.is_terminal());
        assert!(ExecutionState::StartFailed.is_terminal());
        assert!(ExecutionState::Unknown.is_terminal());
        assert!(SandboxState::Deleted.is_terminal());
        assert!(!OperationState::Pending.is_terminal());
        assert!(!SandboxState::Running.is_terminal());
    }
}
