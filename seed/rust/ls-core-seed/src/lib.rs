//! Typed state/CAS and shell-free runc argument construction reference.
//! Not a runtime: no processes, mounts, namespaces, or cgroups are created.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error { InvalidId, StaleRevision, InvalidTransition, Overflow, InvalidRoot }

/// Backend-generated identifier. Never use a user-provided container name as a
/// filesystem path, runc identifier, CNI name, or a shell fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeId(String);
impl RuntimeId {
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty() || value.len() > 64 ||
           !value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
            return Err(Error::InvalidId);
        }
        if !value.as_bytes()[0].is_ascii_alphanumeric() { return Err(Error::InvalidId); }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Prepared, Running, Succeeded, Failed, Unknown }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation { phase: Phase, revision: u64 }
impl Default for Operation {
    fn default() -> Self { Self { phase: Phase::Prepared, revision: 0 } }
}
impl Operation {
    pub fn phase(&self) -> Phase { self.phase }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn transition(&mut self, expected: u64, target: Phase) -> Result<(), Error> {
        if self.revision != expected { return Err(Error::StaleRevision); }
        let allowed = matches!((self.phase, target),
            (Phase::Prepared, Phase::Running | Phase::Failed) |
            (Phase::Running, Phase::Succeeded | Phase::Failed | Phase::Unknown));
        if !allowed { return Err(Error::InvalidTransition); }
        let next_revision = self.revision.checked_add(1).ok_or(Error::Overflow)?;
        self.phase = target;
        self.revision = next_revision;
        Ok(())
    }
}

/// Operator-owned paths. A real privileged helper must validate inode ownership,
/// permissions and symlinks using dirfd-based operations before opening files.
/// This lexical check by itself is NOT a filesystem security boundary.
#[derive(Clone, Debug)]
pub struct StatePaths { root: PathBuf }
impl StatePaths {
    pub fn new(operator_root: &Path) -> Result<Self, Error> {
        if !operator_root.is_absolute() || operator_root.components().any(|c|
            matches!(c, std::path::Component::ParentDir)) {
            return Err(Error::InvalidRoot);
        }
        Ok(Self { root: operator_root.to_owned() })
    }
    pub fn process_file(&self, sandbox: &RuntimeId, execution: &RuntimeId) -> PathBuf {
        self.root.join("operations").join(sandbox.as_str()).join(format!("{}.json", execution.as_str()))
    }
    /// The returned vector is passed directly to std::process::Command.args().
    /// Guest command/env/user belong in the OCI process JSON, never in a host sh.
    pub fn runc_exec_args(&self, sandbox: &RuntimeId, execution: &RuntimeId) -> Vec<OsString> {
        vec!["--root".into(), self.root.join("runc").into_os_string(), "exec".into(),
             "--process".into(), self.process_file(sandbox, execution).into_os_string(),
             sandbox.as_str().into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_untrusted_identifiers() {
        for value in ["", "../x", "/etc", "x y", "x;id", "-flag", "a\0b"] {
            assert_eq!(RuntimeId::parse(value), Err(Error::InvalidId));
        }
    }
    #[test]
    fn stale_revision_does_not_mutate() {
        let mut op = Operation::default();
        assert_eq!(op.transition(9, Phase::Running), Err(Error::StaleRevision));
        assert_eq!(op.phase(), Phase::Prepared);
    }
    #[test]
    fn unknown_cannot_be_retried_as_prepared() {
        let mut op = Operation::default();
        op.transition(0, Phase::Running).unwrap();
        op.transition(1, Phase::Unknown).unwrap();
        assert_eq!(op.transition(2, Phase::Prepared), Err(Error::InvalidTransition));
    }
    #[test]
    fn completed_cannot_run_again() {
        let mut op = Operation::default();
        op.transition(0, Phase::Running).unwrap();
        op.transition(1, Phase::Succeeded).unwrap();
        assert_eq!(op.transition(2, Phase::Running), Err(Error::InvalidTransition));
    }
    #[test]
    fn argument_vector_is_not_shell_text() {
        let p = StatePaths::new(Path::new("/run/light sandbox")).unwrap();
        let s = RuntimeId::parse("sb-01").unwrap();
        let e = RuntimeId::parse("ex-01").unwrap();
        let args = p.runc_exec_args(&s, &e);
        assert_eq!(args.len(), 6);
        assert_eq!(args[2], OsString::from("exec"));
        assert_eq!(args[4], OsString::from("/run/light sandbox/operations/sb-01/ex-01.json"));
    }
    #[test]
    fn rejects_relative_operator_root() {
        assert!(StatePaths::new(Path::new("tmp/state")).is_err());
        assert!(StatePaths::new(Path::new("/run/../etc")).is_err());
    }
}
