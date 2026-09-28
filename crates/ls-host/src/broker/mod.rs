//! T14 — PTYBroker ownership model: pure-logic registry of who holds each
//! PTY master, who may attach, and what "Lost" honestly means
//! (plans/tasks/T14.md; crate::pty carries the design-freeze vocabulary;
//! runc console-socket semantics are the fixed production input).
//!
//! ========================================================================
//! DESIGN FREEZE (T14, card-locked; changes need the contract owner)
//! ========================================================================
//!
//! * ONE OWNER: exactly one [`PtyBroker`] handle exists per live PTY id —
//!   the stand-in for the broker PROCESS that holds the master fd.
//!   Constructing a second handle for a live id is `AlreadyRegistered`.
//! * ATTACH WITH IDENTITY: [`BrokerRegistry::attach`] requires the exact
//!   [`PtyRef`] (id + generation — stale generation is explicitly rejected,
//!   never "helpfully" redirected) and a `PeerCred` whose uid matches the
//!   registered owner. pid is deliberately NOT part of `PeerCred`: PIDs are
//!   not stable security identity (03_DOMAIN). The worker leg maps this to
//!   SO_PEERCRED on the unix socket.
//! * DEATH IS FINAL: dropping the broker handle without `close()` means
//!   the broker died (kill/crash). The id moves to the burned set as
//!   `Lost { BrokerDied }`, its fds are released, and every attach from
//!   then on answers the typed `Lost` — NEVER a transparent re-open. A
//!   clean `close()` burns the id as `Closed`, a different answer from
//!   `Lost`. In both cases the id is never reusable (03_DOMAIN's
//!   deleted-ids-never-recycle rule), so re-registration with the same
//!   PtyRef — at ANY generation — is `BurnedRef`. No handle, no master:
//!   fds are never fabricated from stored integers.
//! * REATTACH CONTINUES THE SAME PTY: a new attach of a LIVE PTY gets a
//!   fresh attach seq and replays the broker's recorded size
//!   ([`AttachGrant::size_at_attach`]); input/output byte accounting is
//!   cumulative per PTY, so a client reconnect observes continuous offsets
//!   (the real offset rules are the T12 spool's; this layer proves the
//!   broker side never resets them).
//! * TYPED CONTROL ONLY: `apply_control` accepts the closed
//!   [`PtyControlFrame`] vocabulary; `write_input` counts bytes and never
//!   inspects them (the no-byte-sniffing freeze in crate::pty is enforced
//!   here by construction — there is no code path that looks at payload).
//!
//! Production legs (concrete unix-socket identity, API-restart reattach,
//! broker-kill recovery, /proc fd pressure) are worker-gated and registered
//! in tests/runtime/pty/EXPECTATIONS.md.

use crate::pty::{
    ControlEffect, FdError, FdLedger, FdLease, PtyControlFrame, PtyRef, TerminalState,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// Fd slots charged for the PTY master side, per PTY.
pub const MASTER_FDS: usize = 1;
/// Fd slots charged for one attachment's local channel.
pub const CHANNEL_FDS: usize = 1;
/// Node-wide fd budget default for the broker registry.
pub const DEFAULT_FD_CAP: usize = 256;
/// Initial window size when a PTY is registered.
pub const DEFAULT_ROWS: u32 = 24;
pub const DEFAULT_COLS: u32 = 80;

// ------------------------------------------------------------------- peers

/// Identity presented at attach. uid-only on purpose: gid travels for
/// completeness/audit, but the MATCH is uid (the PTY owner's uid), and pid
/// is absent because PIDs are not stable identity (03_DOMAIN).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub uid: u32,
    pub gid: u32,
}

// ------------------------------------------------------------------ errors

/// Why a PTY id is gone (burned = never reusable, at any generation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BurnedReason {
    /// The owner died without a clean close; the master fd died with it.
    Lost { reason: LostReason },
    /// Explicit clean teardown through PtyBroker::close.
    Closed,
}

/// Lost has a REASON taxonomy (extendable by the owning task chain); a
/// crashed/killed broker is the first member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostReason {
    BrokerDied,
}

/// Final record of a burned id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnRecord {
    pub generation: u64,
    pub reason: BurnedReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// Id was used before and is never reusable (Lost or Closed).
    BurnedRef(BurnRecord),
    /// Id is live right now; exactly one owner is allowed.
    AlreadyRegistered,
    /// Node fd budget refused the master charge.
    Fd(FdError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// No record of this id at all.
    NotFound,
    /// Id is live but the caller's generation is stale (fencing).
    GenerationMismatch { recorded: u64, requested: u64 },
    /// Peer uid is not the registered owner's uid.
    IdentityMismatch { owner_uid: u32, presenter_uid: u32 },
    /// The broker is dead; the PTY died with it. Never transparently
    /// recovered.
    Lost { reason: LostReason },
    /// Cleanly closed; a deliberate answer, different from Lost.
    Closed,
    /// Node fd budget refused the channel charge.
    Fd(FdError),
}

/// Errors on live operations driven through an existing handle/grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    Lost { reason: LostReason },
    /// The grant's attachment is not live on this broker (detached
    /// already, wrong broker, or stale generation).
    Detached { attach_seq: u64 },
    /// This attachment's INPUT direction is half-closed; output lives.
    InputClosed { attach_seq: u64 },
}

macro_rules! impl_display {
    ($t:ty) => {
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{self:?}")
            }
        }
    };
}
impl_display!(RegisterError);
impl_display!(AttachError);
impl_display!(BrokerError);
impl std::error::Error for RegisterError {}
impl std::error::Error for AttachError {}
impl std::error::Error for BrokerError {}

// ------------------------------------------------------------------ views

/// Live read model for inspection/reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerView {
    pub generation: u64,
    pub owner_uid: u32,
    pub attachments: usize,
    pub size: (u16, u16),
    pub interrupts: u64,
    pub stdin_open: bool,
    pub bytes_in_total: u64,
    pub bytes_out_total: u64,
}

/// Closed lookup vocabulary: unknown vs live vs burned are three explicit
/// answers (burned keeps its honesty record: Lost-with-reason or Closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerLookup {
    Unknown,
    Live(BrokerView),
    Burned(BurnRecord),
}

// ---------------------------------------------------------------- registry

struct RegistryInner {
    live: HashMap<String, BrokerEntry>,
    burned: HashMap<String, BurnRecord>,
    ledger: Arc<Mutex<FdLedger>>,
    next_attach_seq: u64,
}

struct BrokerEntry {
    generation: u64,
    owner_uid: u32,
    term: TerminalState,
    #[allow(dead_code)] // held for RAII, read only by the ledger
    master: FdLease,
    attachments: HashMap<u64, Attachment>,
    bytes_in_total: u64,
    bytes_out_total: u64,
}

struct Attachment {
    input_open: bool,
    #[allow(dead_code)] // held for RAII, read only by the ledger
    channel: FdLease,
}

impl BrokerEntry {
    fn view(&self) -> BrokerView {
        BrokerView {
            generation: self.generation,
            owner_uid: self.owner_uid,
            attachments: self.attachments.len(),
            size: self.term.size(),
            interrupts: self.term.interrupts(),
            stdin_open: self.term.stdin_open(),
            bytes_in_total: self.bytes_in_total,
            bytes_out_total: self.bytes_out_total,
        }
    }
}

impl RegistryInner {
    /// The entry for a live handle, or the honest death answer. Used by
    /// PtyBroker ops: while the caller holds a handle, entry absence means
    /// the broker died between operations — report Lost, never guess.
    fn live_entry(&mut self, id: &str, generation: u64) -> Result<&mut BrokerEntry, BrokerError> {
        match self.live.get_mut(id) {
            Some(e) if e.generation == generation => Ok(e),
            _ => Err(BrokerError::Lost {
                reason: LostReason::BrokerDied,
            }),
        }
    }

    /// Burn a live id if present: remove the entry (leases release by RAII)
    /// and write the honesty record. Returns false when nothing was live.
    fn burn(&mut self, id: &str, record: BurnRecord) -> bool {
        let removed = self.live.remove(id);
        if removed.is_some() {
            drop(removed); // leases release here, deterministically
            self.burned.insert(id.to_string(), record);
            true
        } else {
            false
        }
    }
}

/// Node-level owner of every PTY broker registration. Pure logic; the
/// production worker wraps the same laws around real unix-socket peers.
pub struct BrokerRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

impl BrokerRegistry {
    pub fn new(fd_cap: usize) -> BrokerRegistry {
        BrokerRegistry {
            inner: Arc::new(Mutex::new(RegistryInner {
                live: HashMap::new(),
                burned: HashMap::new(),
                ledger: FdLedger::shared(fd_cap),
                next_attach_seq: 0,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().expect("broker registry poisoned")
    }

    pub fn fds_live(&self) -> usize {
        self.lock().ledger.lock().expect("fd ledger poisoned").live()
    }

    /// Register a PTY under `owner_uid`; returns the ONE owner handle.
    pub fn register(&self, pty: PtyRef, owner_uid: u32) -> Result<PtyBroker, RegisterError> {
        let mut inner = self.lock();
        if let Some(record) = inner.burned.get(pty.id()) {
            return Err(RegisterError::BurnedRef(record.clone()));
        }
        if inner.live.contains_key(pty.id()) {
            return Err(RegisterError::AlreadyRegistered);
        }
        let master = FdLedger::acquire(&inner.ledger, &pty, MASTER_FDS).map_err(RegisterError::Fd)?;
        let term = TerminalState::new(DEFAULT_ROWS, DEFAULT_COLS)
            .expect("default window dims are within bounds");
        let generation = pty.generation();
        let id = pty.id().to_string();
        inner.live.insert(
            id.clone(),
            BrokerEntry {
                generation,
                owner_uid,
                term,
                master,
                attachments: HashMap::new(),
                bytes_in_total: 0,
                bytes_out_total: 0,
            },
        );
        Ok(PtyBroker {
            inner: Arc::clone(&self.inner),
            id,
            generation,
            clean_close: false,
        })
    }

    /// Attach to a live PTY with an identity check. Same PtyRef (id AND
    /// generation) only; anything else is an explicit typed rejection.
    pub fn attach(&self, pty: &PtyRef, cred: PeerCred) -> Result<AttachGrant, AttachError> {
        let mut inner = self.lock();
        let (generation, owner_uid) = match inner.live.get(pty.id()) {
            Some(e) => (e.generation, e.owner_uid),
            None => {
                return match inner.burned.get(pty.id()) {
                    Some(BurnRecord {
                        reason: BurnedReason::Lost { reason },
                        ..
                    }) => Err(AttachError::Lost { reason: *reason }),
                    Some(BurnRecord {
                        reason: BurnedReason::Closed,
                        ..
                    }) => Err(AttachError::Closed),
                    None => Err(AttachError::NotFound),
                };
            }
        };
        if generation != pty.generation() {
            return Err(AttachError::GenerationMismatch {
                recorded: generation,
                requested: pty.generation(),
            });
        }
        if owner_uid != cred.uid {
            return Err(AttachError::IdentityMismatch {
                owner_uid,
                presenter_uid: cred.uid,
            });
        }
        // Only identity-checked attaches charge the channel budget.
        let channel = FdLedger::acquire(&inner.ledger, pty, CHANNEL_FDS).map_err(AttachError::Fd)?;
        let seq = inner.next_attach_seq;
        inner.next_attach_seq += 1;
        let e = inner.live.get_mut(pty.id()).expect("checked live above");
        e.attachments.insert(
            seq,
            Attachment {
                input_open: true,
                channel,
            },
        );
        Ok(AttachGrant {
            inner: Arc::clone(&self.inner),
            id: pty.id().to_string(),
            generation,
            attach_seq: seq,
            size_at_attach: e.term.size(),
        })
    }

    pub fn inspect(&self, id: &str) -> BrokerLookup {
        let inner = self.lock();
        if let Some(e) = inner.live.get(id) {
            return BrokerLookup::Live(e.view());
        }
        if let Some(record) = inner.burned.get(id) {
            return BrokerLookup::Burned(record.clone());
        }
        BrokerLookup::Unknown
    }
}

impl Default for BrokerRegistry {
    fn default() -> Self {
        BrokerRegistry::new(DEFAULT_FD_CAP)
    }
}

// ------------------------------------------------------------------ broker

/// The broker-process-side handle. While this value exists, the PTY is
/// owned. Dropping it without [`PtyBroker::close`] IS the crash model: the
/// id burns as Lost and the fds release — no re-open, ever.
pub struct PtyBroker {
    inner: Arc<Mutex<RegistryInner>>,
    id: String,
    generation: u64,
    clean_close: bool,
}

impl std::fmt::Debug for PtyBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyBroker")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .finish()
    }
}

impl PtyBroker {
    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner.lock().expect("broker registry poisoned")
    }

    pub fn pty_ref(&self) -> PtyRef {
        PtyRef::new(&self.id, self.generation).expect("registered refs are valid")
    }

    /// Terminal-global control: resize / window-change / Ctrl-C / master
    /// stdin half-close. Always a typed frame, never a byte.
    pub fn apply_control(&self, frame: PtyControlFrame) -> Result<ControlEffect, BrokerError> {
        let mut inner = self.lock();
        let e = inner.live_entry(&self.id, self.generation)?;
        Ok(e.term.apply(&frame))
    }

    /// The grant belongs to THIS broker and generation, else it is a
    /// detached foreign reference, never silently accepted.
    fn check_grant(e: &BrokerEntry, id: &str, generation: u64, grant: &AttachGrant) -> Result<(), BrokerError> {
        let _ = e;
        if grant.id != id || grant.generation != generation {
            return Err(BrokerError::Detached {
                attach_seq: grant.attach_seq,
            });
        }
        Ok(())
    }

    /// Payload input from one attachment: counted, NEVER inspected.
    pub fn write_input(&self, grant: &AttachGrant, bytes: &[u8]) -> Result<(), BrokerError> {
        let mut inner = self.lock();
        let e = inner.live_entry(&self.id, self.generation)?;
        Self::check_grant(e, &self.id, self.generation, grant)?;
        match e.attachments.get(&grant.attach_seq) {
            None => Err(BrokerError::Detached {
                attach_seq: grant.attach_seq,
            }),
            Some(a) if !a.input_open || !e.term.stdin_open() => Err(BrokerError::InputClosed {
                attach_seq: grant.attach_seq,
            }),
            Some(_) => {
                e.bytes_in_total += bytes.len() as u64;
                Ok(())
            }
        }
    }

    /// One producer output write reaches every live attachment; returns
    /// how many attachments it was delivered to. Counted, never inspected.
    pub fn emit_output(&self, bytes: &[u8]) -> Result<usize, BrokerError> {
        let mut inner = self.lock();
        let e = inner.live_entry(&self.id, self.generation)?;
        e.bytes_out_total += bytes.len() as u64;
        Ok(e.attachments.len())
    }

    /// Half-close ONE attachment's input direction. The master stdin
    /// reaches EOF only after the LAST input-capable attachment closes.
    pub fn half_close(&self, grant: &AttachGrant) -> Result<(), BrokerError> {
        let mut inner = self.lock();
        let e = inner.live_entry(&self.id, self.generation)?;
        Self::check_grant(e, &self.id, self.generation, grant)?;
        let a = e
            .attachments
            .get_mut(&grant.attach_seq)
            .ok_or(BrokerError::Detached {
                attach_seq: grant.attach_seq,
            })?;
        if !a.input_open {
            return Ok(()); // idempotent: re-close is not an error
        }
        a.input_open = false;
        let any_input_open = e.attachments.values().any(|x| x.input_open);
        if !any_input_open && e.term.stdin_open() {
            e.term.apply(&PtyControlFrame::half_close());
        }
        Ok(())
    }

    /// Clean teardown: burns the id as Closed and releases every lease.
    pub fn close(mut self) -> Result<(), BrokerError> {
        let mut inner = self.lock();
        let ok = inner.burn(
            &self.id,
            BurnRecord {
                generation: self.generation,
                reason: BurnedReason::Closed,
            },
        );
        drop(inner);
        if !ok {
            return Err(BrokerError::Lost {
                reason: LostReason::BrokerDied,
            });
        }
        self.clean_close = true;
        Ok(())
    }
}

impl Drop for PtyBroker {
    fn drop(&mut self) {
        if self.clean_close {
            return;
        }
        // Crash model: an unclosed handle is a dead broker. Never poison-
        // panic here — a poisoned registry during unwind must not abort the
        // process on top of whatever already failed.
        if let Ok(mut inner) = self.inner.lock() {
            inner.burn(
                &self.id,
                BurnRecord {
                    generation: self.generation,
                    reason: BurnedReason::Lost {
                        reason: LostReason::BrokerDied,
                    },
                },
            );
        }
    }
}

// ------------------------------------------------------------------ attach

/// One live attachment. Dropping detaches (releases the channel lease);
/// `detach()` is the explicit spelling of the same thing.
pub struct AttachGrant {
    inner: Arc<Mutex<RegistryInner>>,
    id: String,
    generation: u64,
    attach_seq: u64,
    size_at_attach: (u16, u16),
}

impl std::fmt::Debug for AttachGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachGrant")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("attach_seq", &self.attach_seq)
            .finish()
    }
}

impl AttachGrant {
    pub fn attach_seq(&self) -> u64 {
        self.attach_seq
    }

    /// The broker-recorded window size replayed at attach time (client
    /// reattach continuity rule).
    pub fn size_at_attach(&self) -> (u16, u16) {
        self.size_at_attach
    }

    pub fn detach(self) {
        // Drop does the deterministic work.
    }
}

impl Drop for AttachGrant {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(e) = inner.live.get_mut(&self.id) {
                if e.generation == self.generation {
                    e.attachments.remove(&self.attach_seq);
                }
            }
        }
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(uid: u32) -> PeerCred {
        PeerCred { uid, gid: uid }
    }

    fn p(id: &str, gen: u64) -> PtyRef {
        PtyRef::new(id, gen).unwrap()
    }

    #[test]
    fn register_then_attach_with_owner_identity() {
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        assert_eq!(reg.fds_live(), MASTER_FDS, "master charged");
        let g0 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(g0.attach_seq(), 0, "attach seqs are registry-absolute");
        assert_eq!(g0.size_at_attach(), (DEFAULT_ROWS as u16, DEFAULT_COLS as u16));
        assert_eq!(reg.fds_live(), MASTER_FDS + CHANNEL_FDS);
        let g1 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(g1.attach_seq(), 1);
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => {
                assert_eq!(v.generation, 1);
                assert_eq!(v.owner_uid, 1000);
                assert_eq!(v.attachments, 2);
            }
            other => panic!("expected live: {other:?}"),
        }
        drop(g1);
        assert_eq!(reg.fds_live(), MASTER_FDS + CHANNEL_FDS, "detach releases the channel");
        drop(g0);
        drop(broker);
        assert_eq!(reg.fds_live(), 0, "every lease returned after death");
    }

    #[test]
    fn attach_wrong_generation_is_explicit_fencing_rejection() {
        let reg = BrokerRegistry::default();
        let _broker = reg.register(p("t1", 7), 1000).unwrap();
        let before = reg.fds_live();
        assert_eq!(reg.attach(&p("t1", 6), cred(1000)).unwrap_err(), AttachError::GenerationMismatch { recorded: 7, requested: 6 },
            "stale generation rejected, never redirected");
        assert_eq!(reg.attach(&p("t1", 8), cred(1000)).unwrap_err(), AttachError::GenerationMismatch { recorded: 7, requested: 8 });
        assert_eq!(reg.fds_live(), before, "a rejected attach charged nothing");
        assert_eq!(reg.attach(&p("nobody", 1), cred(1000)).unwrap_err(), AttachError::NotFound);
    }

    #[test]
    fn attach_identity_check_matches_owner_uid() {
        let reg = BrokerRegistry::default();
        let _broker = reg.register(p("t1", 1), 1000).unwrap();
        assert_eq!(reg.attach(&p("t1", 1), cred(1001)).unwrap_err(), AttachError::IdentityMismatch { owner_uid: 1000, presenter_uid: 1001 });
        // uid match with a different gid attaches (the match is uid-only):
        assert!(
            reg.attach(&p("t1", 1), PeerCred { uid: 1000, gid: 2000 }).is_ok(),
            "gid is carried, not matched"
        );
    }

    #[test]
    fn broker_death_is_honest_lost_and_the_ref_is_burned() {
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        let grant = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        drop(broker); // the crash: no close(), handle just dies
        assert_eq!(reg.fds_live(), 0, "dead broker's fds released (OS truth, modelled)");
        assert_eq!(reg.attach(&p("t1", 1), cred(1000)).unwrap_err(), AttachError::Lost { reason: LostReason::BrokerDied },
            "typed Lost with reason, never transparent recovery");
        assert_eq!(
            stale_grant_check(&reg, &grant),
            Err(BrokerError::Detached { attach_seq: 0 }),
            "pre-death grants are inert: they carry no authority on any other broker"
        );
        drop(grant); // detach after death is a harmless no-op
        assert_eq!(
            reg.register(p("t1", 1), 1000).unwrap_err(),
            RegisterError::BurnedRef(BurnRecord {
                generation: 1,
                reason: BurnedReason::Lost { reason: LostReason::BrokerDied }
            }),
            "re-registration with the same PtyRef is rejected"
        );
        assert_eq!(
            reg.register(p("t1", 2), 1000).unwrap_err(),
            RegisterError::BurnedRef(BurnRecord {
                generation: 1,
                reason: BurnedReason::Lost { reason: LostReason::BrokerDied }
            }),
            "the id never recycles, at any generation"
        );
        match reg.inspect("t1") {
            BrokerLookup::Burned(r) => {
                assert_eq!(r.reason, BurnedReason::Lost { reason: LostReason::BrokerDied })
            }
            other => panic!("expected burned: {other:?}"),
        }
    }

    /// A grant from the dead broker gains nothing on an unrelated live one:
    /// the attachment simply is not live THERE (Detached).
    fn stale_grant_check(reg: &BrokerRegistry, grant: &AttachGrant) -> Result<(), BrokerError> {
        let probe = reg.register(PtyRef::new("probe", 1).unwrap(), 1000).unwrap();
        probe.write_input(grant, b"x")
    }

    #[test]
    fn clean_close_distinct_from_lost() {
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        broker.close().unwrap();
        assert_eq!(reg.fds_live(), 0);
        assert_eq!(reg.attach(&p("t1", 1), cred(1000)).unwrap_err(), AttachError::Closed);
        assert_eq!(
            reg.register(p("t1", 1), 1000).unwrap_err(),
            RegisterError::BurnedRef(BurnRecord {
                generation: 1,
                reason: BurnedReason::Closed
            })
        );
    }

    #[test]
    fn register_never_shadows_a_live_id() {
        let reg = BrokerRegistry::default();
        let _broker = reg.register(p("t1", 1), 1000).unwrap();
        assert_eq!(
            reg.register(p("t1", 1), 1000).unwrap_err(),
            RegisterError::AlreadyRegistered
        );
    }

    #[test]
    fn typed_control_drives_terminal_state_and_reattach_replays_size() {
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        assert_eq!(
            broker.apply_control(PtyControlFrame::resize(40, 120).unwrap()),
            Ok(ControlEffect::SizeSet { rows: 40, cols: 120 })
        );
        assert_eq!(
            broker.apply_control(PtyControlFrame::window_change(41, 121).unwrap()),
            Ok(ControlEffect::SizeAdvisory { rows: 41, cols: 121 })
        );
        assert_eq!(
            broker.apply_control(PtyControlFrame::ctrl_c()),
            Ok(ControlEffect::InterruptForeground)
        );
        let g0 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(g0.size_at_attach(), (41, 121), "attach replays last recorded size");
        drop(g0);
        let g1 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(g1.size_at_attach(), (41, 121), "re-attach: same broker, same PTY, same size");
        assert_eq!(g1.attach_seq(), 1, "new attachment, new seq");
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => {
                assert_eq!(v.size, (41, 121));
                assert_eq!(v.interrupts, 1);
            }
            other => panic!("expected live: {other:?}"),
        }
    }

    #[test]
    fn payload_bytes_are_counted_never_sniffed() {
        // Terminal-byte soup that a sniffer would misread as controls:
        // ETX (0x03 = Ctrl-C), EOT (0x04 = EOF), CSI resize escape
        // (ESC [ 8 ; 40 ; 120 t), CUP, DECAWM — ALL must be inert data.
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        let grant = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        let soup: &[u8] = b"\x03\x04\x1b[8;40;120t\x1b[H\x1b[?25hnormal text\r\n";
        broker.write_input(&grant, soup).unwrap();
        broker.write_input(&grant, soup).unwrap();
        assert_eq!(broker.emit_output(soup).unwrap(), 1);
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => {
                assert_eq!(v.interrupts, 0, "0x03 bytes are data, not Ctrl-C");
                assert_eq!(v.size, (DEFAULT_ROWS as u16, DEFAULT_COLS as u16), "escape did not resize");
                assert!(v.stdin_open, "0x04 did not half-close");
                assert_eq!(v.bytes_in_total, (soup.len() * 2) as u64);
                assert_eq!(v.bytes_out_total, soup.len() as u64);
            }
            other => panic!("expected live: {other:?}"),
        }
    }

    #[test]
    fn half_close_is_per_attachment_and_eof_only_when_last_input_leaves() {
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        let g0 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        let g1 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        broker.half_close(&g0).unwrap();
        assert_eq!(
            broker.write_input(&g0, b"late"),
            Err(BrokerError::InputClosed { attach_seq: 0 }),
            "this attachment's input is done"
        );
        broker.write_input(&g1, b"sibling still types").unwrap();
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => assert!(v.stdin_open, "master stdin lives while a sibling inputs"),
            other => panic!("expected live: {other:?}"),
        }
        // output is unaffected by input half-close — both attachments still:
        assert_eq!(broker.emit_output(b"out").unwrap(), 2);
        broker.half_close(&g1).unwrap();
        broker.half_close(&g1).unwrap(); // idempotent
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => assert!(!v.stdin_open, "last input close reaches master EOF"),
            other => panic!("expected live: {other:?}"),
        }
        drop(g1);
        let g2 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(g2.attach_seq(), 2, "global attach seqs keep counting");
        assert_eq!(
            broker.write_input(&g2, b"too late"),
            Err(BrokerError::InputClosed { attach_seq: 2 }),
            "master stdin EOF is irrevocable, also for later attachments"
        );
        assert_eq!(
            broker.emit_output(b"out2").unwrap(),
            2,
            "output still reaches the surviving and the new attachment"
        );
    }

    #[test]
    fn fd_cap_pressure_is_explicit_never_silent() {
        let reg = BrokerRegistry::new(MASTER_FDS + CHANNEL_FDS);
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        let g0 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(
            reg.attach(&p("t1", 1), cred(1000)).unwrap_err(),
            AttachError::Fd(FdError { live: 2, cap: 2, requested: 1 }),
            "exhaustion carries the honest numbers"
        );
        drop(g0);
        let g1 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(reg.fds_live(), 2, "budget freed by detach, re-attach reclaims");
        assert_eq!(
            reg.register(p("t2", 1), 1000).unwrap_err(),
            RegisterError::Fd(FdError { live: 2, cap: 2, requested: 1 }),
            "master also budget-checked"
        );
        drop(broker);
        assert_eq!(reg.fds_live(), 0, "death reclaims under pressure too");
        drop(g1); // detach after death is a no-op, never a panic
        assert!(reg.register(p("t3", 1), 1000).is_ok());
    }

    #[test]
    fn sibling_ptys_never_share_fate() {
        // T14-C04 vocabulary arm: killing/resizing A must not move B.
        let reg = BrokerRegistry::default();
        let a = reg.register(p("a", 1), 1000).unwrap();
        let _b = reg.register(p("b", 1), 1000).unwrap();
        a.apply_control(PtyControlFrame::resize(50, 200).unwrap()).unwrap();
        a.apply_control(PtyControlFrame::ctrl_c()).unwrap();
        drop(a); // A dies
        assert_eq!(reg.attach(&p("a", 1), cred(1000)).unwrap_err(), AttachError::Lost { reason: LostReason::BrokerDied });
        let gb = reg.attach(&p("b", 1), cred(1000)).unwrap();
        _b.write_input(&gb, b"b lives").unwrap();
        match reg.inspect("b") {
            BrokerLookup::Live(v) => {
                assert_eq!(v.size, (DEFAULT_ROWS as u16, DEFAULT_COLS as u16), "A's resize did not leak");
                assert_eq!(v.interrupts, 0, "A's Ctrl-C did not leak");
                assert_eq!(v.bytes_in_total, 7);
            }
            other => panic!("expected live: {other:?}"),
        }
    }

    #[test]
    fn reattach_after_client_disconnect_continues_same_pty() {
        // T14-C02 vocabulary arm: client disconnect = detach; the API (or
        // the same client later) reattaches into the SAME broker/PTY with
        // continuous counters and replayed size — no new shell, no reset.
        let reg = BrokerRegistry::default();
        let broker = reg.register(p("t1", 1), 1000).unwrap();
        let c1 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        broker.write_input(&c1, b"make-ten..").unwrap();
        broker.apply_control(PtyControlFrame::resize(30, 100).unwrap()).unwrap();
        c1.detach(); // client disconnect
        let c2 = reg.attach(&p("t1", 1), cred(1000)).unwrap();
        assert_eq!(c2.attach_seq(), 1);
        assert_eq!(c2.size_at_attach(), (30, 100), "size replay survives disconnect");
        broker.write_input(&c2, b"more").unwrap();
        match reg.inspect("t1") {
            BrokerLookup::Live(v) => assert_eq!(v.bytes_in_total, 14, "input offsets continuous"),
            other => panic!("expected live: {other:?}"),
        }
    }
}
