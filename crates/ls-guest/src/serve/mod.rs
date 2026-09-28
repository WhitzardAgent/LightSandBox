//! Guest control agent (`lsguest serve --fd <N>`) — the ls-guest role upgrade
//! of ADR 0002 §8 (round-05 directive §9–13): a tiny control channel the
//! Kernel/Machine InstanceSupervisor talks to over one framed byte stream
//! (in production the guest serial line; in tests a socketpair).
//!
//! * [`protocol`] — wire format v1 and the fail-closed codec;
//! * [`exec`] — one-at-a-time child exec with piped stdout/stderr, bounded
//!   output backpressure, timeout/cancel/signal termination;
//! * [`agent`] — the control loop (`Agent::run`), serve args, fd handoff.

#![forbid(unsafe_code)]

pub mod agent;
pub mod exec;
pub mod protocol;

pub use agent::Agent;
