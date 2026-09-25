//! T04 domain core: typed identities, ownership/fencing, idempotency,
//! lifecycle state machines, operation/execution semantics, resource
//! arithmetic, the typed API↔manager control protocol, and migration
//! decisions. Pure domain: no I/O, no subprocess, no SQL execution — see
//! docs/v5/02_ARCHITECTURE.md §模块与依赖方向.

pub mod access;
pub mod backend;
pub mod control;
pub mod error;
pub mod execution;
pub mod idempotency;
pub mod ids;
pub mod lifecycle;
pub mod migration;
pub mod operation;
pub mod resources;

/// Load any repo-relative JSON document (test fixture driver). Production
/// domain logic never reads files; this exists for contract-lock tests, like
/// `contracts::fixture_json` but rooted at the repository root so T04-owned
/// fixtures under tests/ can be consumed.
#[cfg(test)]
pub(crate) fn repo_file_json(rel: &str) -> serde_json::Value {
    use std::path::PathBuf;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {rel} unreadable: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {rel} invalid: {e}"))
}
