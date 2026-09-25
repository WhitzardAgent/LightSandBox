//! T08 runtime slice: backend abstraction, durable journal with reverse
//! rollback, cleanup ledger, the sandbox manager (create/inspect/exec/delete
//! minimal API), and the native runc backend.
//!
//! Wiring only — behaviour lives in the submodules. Declaration order is
//! dependency order (`backend` is consumed by `manager` and `native`).

pub mod backend;
pub mod cleanup;
pub mod journal;
pub mod manager;
pub mod native;
