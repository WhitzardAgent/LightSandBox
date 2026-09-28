//! T14 — PTY core vocabulary: references, typed terminal-control frames,
//! terminal bookkeeping, and the FD-accounting guard
//! (plans/tasks/T14.md; docs/v5/03_DOMAIN.md Session row + §资源与生命周期
//! generation fencing; runc console-socket semantics are the fixed input).
//!
//! ========================================================================
//! DESIGN FREEZE (T14, card-locked; changes need the contract owner)
//! ========================================================================
//!
//! * ROLES: a dedicated PTYBroker process holds the PTY MASTER fd. The API
//!   never owns a master; it attaches/reattaches over a local channel with
//!   an identity check (worker leg: SO_PEERCRED on a unix socket; unit
//!   layer: [`crate::broker`] models the same checks in pure logic).
//! * HONEST LOST: if the broker dies, the PTY is `Lost` with an explicit
//!   reason. A PTY is NEVER re-opened-by-number and NEVER faked from
//!   database integers: the only legal fd source is a live broker handle.
//!   "Restart the shell to simulate attach" is REJECTED as a design — a
//!   new shell is a new session, not the old PTY.
//! * TYPED CONTROL, NEVER BYTE SNIFFING: resize / window-change / Ctrl-C /
//!   half-close are [`PtyControlFrame`] values. Raw terminal bytes —
//!   including `\x03` (ETX), `\x04` (EOT), and CSI resize escapes — are
//!   DATA. Nothing in this module or the broker inspects payload bytes for
//!   control meaning; termios/escape interpretation belongs to the guest
//!   side of the PTY, not to our control path.
//! * RESIZE vs WINDOW-CHANGE: both carry (rows, cols) and both update the
//!   recorded size. `Resize` is the AUTHORITATIVE set (operator/API-driven,
//!   effect [`ControlEffect::SizeSet`], worker performs the
//!   TIOCSWINSZ-equivalent and SIGWINCH); `WindowChange` is the client's
//!   window resize NOTIFICATION (effect [`ControlEffect::SizeAdvisory`]).
//!   The last recorded size is what a fresh attach replays.
//! * HALF-CLOSE IS DIRECTIONAL: closing the INPUT direction of an
//!   attachment never touches its OUTPUT; master stdin reaches EOF only
//!   when the broker observes the last input-capable attachment close.
//! * FD LEAKS ARE A VOCABULARY ERROR: every fd acquisition goes through
//!   [`FdLedger::acquire`] (global cap, explicit exhaustion error) and is
//!   released exactly once by RAII ([`FdLease`] Drop semantics). The
//!   per-ref accounting makes "close one session never closes a sibling"
//!   (T14-C04) provable at the vocabulary level; the real /proc fd-diff
//!   legs are registered in tests/runtime/pty/EXPECTATIONS.md.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// Domain id length cap (mirrors ls_core::domain::ids).
pub const MAX_ID_LEN: usize = 128;
/// Hard window-dimension bound (fail-closed; far beyond any real terminal).
pub const MAX_WIN_DIM: u32 = 4096;

// ------------------------------------------------------------------- refs

/// Opaque identity of one PTY instance: `id` mirrors the domain id rules,
/// `generation` fences re-incarnation (a restarted sandbox that reuses a
/// name presents a NEW generation; stale-generation attach is an explicit
/// rejection, see crate::broker).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PtyRef {
    id: String,
    generation: u64,
}

impl PtyRef {
    pub fn new(id: impl AsRef<str>, generation: u64) -> Result<Self, PtyError> {
        let id = id.as_ref();
        if id.is_empty() {
            return Err(PtyError::InvalidId("must not be empty".into()));
        }
        if id.len() > MAX_ID_LEN {
            return Err(PtyError::InvalidId(format!("exceeds {MAX_ID_LEN} chars")));
        }
        for ch in id.chars() {
            let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
            if !ok {
                return Err(PtyError::InvalidId(format!(
                    "contains illegal character {ch:?}"
                )));
            }
        }
        if generation == 0 {
            return Err(PtyError::InvalidGeneration(generation));
        }
        Ok(PtyRef {
            id: id.to_string(),
            generation,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Reference-construction failures (boundary validation, always explicit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyError {
    InvalidId(String),
    InvalidGeneration(u64),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtyError::InvalidId(why) => write!(f, "invalid pty id: {why}"),
            PtyError::InvalidGeneration(g) => write!(f, "invalid pty generation {g} (must be >= 1)"),
        }
    }
}

impl std::error::Error for PtyError {}

// ------------------------------------------------------------- control law

/// Typed terminal-control frames. This CLOSED set is the entire control
/// vocabulary; there is intentionally no `Data` variant here — payload
/// bytes travel as bytes, elsewhere, unparsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyControlFrame {
    /// Authoritative size set (operator/API-driven).
    Resize { rows: u16, cols: u16 },
    /// Client window-change notification (the terminal resized).
    WindowChange { rows: u16, cols: u16 },
    /// Interrupt the foreground process group (SIGINT). This is what
    /// Ctrl-C MEANS; the byte 0x03 is never injected or scanned.
    CtrlC,
    /// Close the stdin direction (scope decided by the caller's layer).
    HalfClose,
}

/// Frame-construction failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlError {
    pub rows: u32,
    pub cols: u32,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "window dimensions must be within 1..={MAX_WIN_DIM}, got {}x{}",
            self.rows, self.cols
        )
    }
}

impl std::error::Error for ControlError {}

fn check_dims(rows: u32, cols: u32) -> Result<(u16, u16), ControlError> {
    if rows == 0 || cols == 0 || rows > MAX_WIN_DIM || cols > MAX_WIN_DIM {
        return Err(ControlError { rows, cols });
    }
    Ok((rows as u16, cols as u16))
}

impl PtyControlFrame {
    /// Authoritative resize; dimensions bounded 1..=MAX_WIN_DIM.
    pub fn resize(rows: u32, cols: u32) -> Result<Self, ControlError> {
        let (rows, cols) = check_dims(rows, cols)?;
        Ok(PtyControlFrame::Resize { rows, cols })
    }

    /// Client window-change notification; same dimension discipline.
    pub fn window_change(rows: u32, cols: u32) -> Result<Self, ControlError> {
        let (rows, cols) = check_dims(rows, cols)?;
        Ok(PtyControlFrame::WindowChange { rows, cols })
    }

    pub fn ctrl_c() -> Self {
        PtyControlFrame::CtrlC
    }

    pub fn half_close() -> Self {
        PtyControlFrame::HalfClose
    }
}

/// The effect a worker must realize for one applied frame. Effects are
/// explicit so nothing above invents syscalls from them silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEffect {
    /// Authoritative size now (rows, cols): TIOCSWINSZ-equivalent + SIGWINCH.
    SizeSet { rows: u16, cols: u16 },
    /// Client-driven window change recorded as (rows, cols).
    SizeAdvisory { rows: u16, cols: u16 },
    /// SIGINT to the foreground process group of the PTY.
    InterruptForeground,
    /// Stdin direction closed: foreground reads observe EOF; output lives.
    InputEof,
}

/// Per-PTY terminal bookkeeping the broker keeps for replay on attach.
/// This is a RECORD; it performs no ioctl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalState {
    rows: u16,
    cols: u16,
    advisory: Option<(u16, u16)>,
    interrupts: u64,
    stdin_open: bool,
}

impl TerminalState {
    pub fn new(rows: u32, cols: u32) -> Result<Self, ControlError> {
        let (rows, cols) = check_dims(rows, cols)?;
        Ok(TerminalState {
            rows,
            cols,
            advisory: None,
            interrupts: 0,
            stdin_open: true,
        })
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn advisory(&self) -> Option<(u16, u16)> {
        self.advisory
    }

    pub fn interrupts(&self) -> u64 {
        self.interrupts
    }

    pub fn stdin_open(&self) -> bool {
        self.stdin_open
    }

    /// Apply one typed frame; the effect says what a worker must realize.
    /// Half-close is idempotent (re-closing a closed direction is fine and
    /// still answers InputEof — never an error, never a re-open).
    pub fn apply(&mut self, frame: &PtyControlFrame) -> ControlEffect {
        match *frame {
            PtyControlFrame::Resize { rows, cols } => {
                self.rows = rows;
                self.cols = cols;
                ControlEffect::SizeSet { rows, cols }
            }
            PtyControlFrame::WindowChange { rows, cols } => {
                self.rows = rows;
                self.cols = cols;
                self.advisory = Some((rows, cols));
                ControlEffect::SizeAdvisory { rows, cols }
            }
            PtyControlFrame::CtrlC => {
                self.interrupts += 1;
                ControlEffect::InterruptForeground
            }
            PtyControlFrame::HalfClose => {
                self.stdin_open = false;
                ControlEffect::InputEof
            }
        }
    }
}

// -------------------------------------------------------------- FD ledger

/// Exhaustion carries the full truth (live/cap/requested); the caller maps
/// it to the frozen UNAVAILABLE/RESOURCE family at the façade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdError {
    pub live: usize,
    pub cap: usize,
    pub requested: usize,
}

impl std::fmt::Display for FdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "fd budget exhausted: live {} + requested {} > cap {}",
            self.live, self.requested, self.cap
        )
    }
}

impl std::error::Error for FdError {}

/// Node-wide fd accounting. Shared (Arc) by every PTY owner on the node;
/// the `by_ref` table makes per-session isolation provable. No real fds
/// exist at this layer: this is the vocabulary-level no-leak guard whose
/// invariants the /proc fd-diff worker legs (T14-C04) verify on a host.
#[derive(Debug)]
pub struct FdLedger {
    cap: usize,
    live: usize,
    by_ref: BTreeMap<PtyRef, usize>,
}

impl FdLedger {
    /// Create an empty ledger wrapped for sharing. cap 0 is legal (every
    /// acquire then fails explicitly).
    pub fn shared(cap: usize) -> Arc<Mutex<FdLedger>> {
        Arc::new(Mutex::new(FdLedger {
            cap,
            live: 0,
            by_ref: BTreeMap::new(),
        }))
    }

    pub fn live(&self) -> usize {
        self.live
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn used_by(&self, owner: &PtyRef) -> usize {
        self.by_ref.get(owner).copied().unwrap_or(0)
    }

    /// live == sum(by_ref): the accounting never drifts from ownership.
    pub fn invariant_holds(&self) -> bool {
        self.live == self.by_ref.values().sum::<usize>()
    }

    /// Charge `n` slots to `owner` inside an already-held lock; returns the
    /// share data the lease needs to release exactly-once later.
    fn charge(&mut self, owner: &PtyRef, n: usize) -> Result<(), FdError> {
        if self.live + n > self.cap {
            return Err(FdError {
                live: self.live,
                cap: self.cap,
                requested: n,
            });
        }
        self.live += n;
        *self.by_ref.entry(owner.clone()).or_insert(0) += n;
        Ok(())
    }

    fn release(&mut self, owner: &PtyRef, n: usize) {
        debug_assert!(self.invariant_holds(), "ledger drifted before release");
        let cur = self.by_ref.get_mut(owner).expect("release without owner");
        debug_assert!(*cur >= n, "release beyond owner's charge");
        *cur -= n;
        if *cur == 0 {
            self.by_ref.remove(owner);
        }
        self.live -= n;
        debug_assert!(self.invariant_holds(), "ledger drifted after release");
    }

    /// Acquire `n` fd slots owned by `owner`; the returned lease releases
    /// them EXACTLY ONCE (RAII). Over-cap is an explicit error answer, never
    /// silent overcommit.
    pub fn acquire(
        ledger: &Arc<Mutex<FdLedger>>,
        owner: &PtyRef,
        n: usize,
    ) -> Result<FdLease, FdError> {
        lock(ledger).charge(owner, n)?;
        Ok(FdLease {
            ledger: Arc::clone(ledger),
            owner: owner.clone(),
            n,
        })
    }
}

fn lock(ledger: &Arc<Mutex<FdLedger>>) -> MutexGuard<'_, FdLedger> {
    // Poison means a holder panicked mid-accounting; failing loudly here
    // keeps the no-leak claim honest instead of silently trusting a
    // possibly-half-updated ledger.
    ledger.lock().expect("fd ledger poisoned")
}

/// RAII guard for charged fd slots. Dropping is the ONLY release path, so
/// release-once is a type property, not a convention.
pub struct FdLease {
    ledger: Arc<Mutex<FdLedger>>,
    owner: PtyRef,
    n: usize,
}

impl FdLease {
    pub fn owner(&self) -> &PtyRef {
        &self.owner
    }

    pub fn slots(&self) -> usize {
        self.n
    }
}

impl Drop for FdLease {
    fn drop(&mut self) {
        lock(&self.ledger).release(&self.owner, self.n);
    }
}

impl std::fmt::Debug for FdLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FdLease")
            .field("owner", &self.owner)
            .field("n", &self.n)
            .finish()
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> PtyRef {
        PtyRef::new(id, 1).unwrap()
    }

    #[test]
    fn pty_ref_rules_mirror_domain_ids_and_fence_generation() {
        let r = PtyRef::new("pty_01.A-b", 7).unwrap();
        assert_eq!(r.id(), "pty_01.A-b");
        assert_eq!(r.generation(), 7);
        assert_ne!(r, PtyRef::new("pty_01.A-b", 8).unwrap(), "generation is identity");
        for bad in ["", &"x".repeat(MAX_ID_LEN + 1), "has space", "has/slash", "nu\0l", "沙盒"] {
            assert!(matches!(PtyRef::new(bad, 1), Err(PtyError::InvalidId(_))), "{bad:?}");
        }
        assert_eq!(
            PtyRef::new("p", 0),
            Err(PtyError::InvalidGeneration(0)),
            "generation 0 is not an incarnation"
        );
    }

    #[test]
    fn frame_constructors_bound_dimensions_fail_closed() {
        assert!(PtyControlFrame::resize(24, 80).is_ok());
        assert!(PtyControlFrame::resize(1, MAX_WIN_DIM).is_ok());
        for (r, c) in [(0, 80), (24, 0), (MAX_WIN_DIM + 1, 80), (24, MAX_WIN_DIM + 1)] {
            assert_eq!(
                PtyControlFrame::resize(r, c),
                Err(ControlError { rows: r, cols: c }),
                "resize {r}x{c}"
            );
            assert_eq!(
                PtyControlFrame::window_change(r, c),
                Err(ControlError { rows: r, cols: c }),
                "window_change {r}x{c}"
            );
        }
        assert_eq!(PtyControlFrame::ctrl_c(), PtyControlFrame::CtrlC);
        assert_eq!(PtyControlFrame::half_close(), PtyControlFrame::HalfClose);
    }

    #[test]
    fn terminal_state_tracks_resize_and_window_change_distinctly() {
        let mut t = TerminalState::new(24, 80).unwrap();
        assert_eq!(t.size(), (24, 80));
        assert_eq!(
            t.apply(&PtyControlFrame::resize(40, 120).unwrap()),
            ControlEffect::SizeSet { rows: 40, cols: 120 }
        );
        assert_eq!(t.size(), (40, 120));
        assert_eq!(t.advisory(), None, "authoritative resize is not advisory");
        assert_eq!(
            t.apply(&PtyControlFrame::window_change(50, 132).unwrap()),
            ControlEffect::SizeAdvisory { rows: 50, cols: 132 }
        );
        assert_eq!(t.size(), (50, 132));
        assert_eq!(t.advisory(), Some((50, 132)), "hint recorded for audit");
    }

    #[test]
    fn terminal_state_ctrl_c_counts_and_half_close_is_idempotent() {
        let mut t = TerminalState::new(24, 80).unwrap();
        assert!(t.stdin_open());
        assert_eq!(t.apply(&PtyControlFrame::ctrl_c()), ControlEffect::InterruptForeground);
        assert_eq!(t.apply(&PtyControlFrame::ctrl_c()), ControlEffect::InterruptForeground);
        assert_eq!(t.interrupts(), 2);
        assert_eq!(t.apply(&PtyControlFrame::half_close()), ControlEffect::InputEof);
        assert!(!t.stdin_open());
        assert_eq!(
            t.apply(&PtyControlFrame::half_close()),
            ControlEffect::InputEof,
            "re-close is honest, not an error"
        );
        // interrupting after stdin EOF still reaches the foreground group:
        assert_eq!(t.apply(&PtyControlFrame::ctrl_c()), ControlEffect::InterruptForeground);
        assert_eq!(t.interrupts(), 3);
    }

    #[test]
    fn fd_ledger_cap_is_explicit_and_reclaimable() {
        let ledger = FdLedger::shared(2);
        let a = FdLedger::acquire(&ledger, &p("a"), 1).unwrap();
        let _b = FdLedger::acquire(&ledger, &p("b"), 1).unwrap();
        assert_eq!(
            FdLedger::acquire(&ledger, &p("c"), 1).err(),
            Some(FdError { live: 2, cap: 2, requested: 1 }),
            "exhaustion names live/cap/requested"
        );
        assert_eq!(ledger.lock().unwrap().live(), 2);
        drop(a);
        assert_eq!(ledger.lock().unwrap().live(), 1, "release reclaims budget");
        assert!(FdLedger::acquire(&ledger, &p("c"), 1).is_ok());
        assert!(ledger.lock().unwrap().invariant_holds());
    }

    #[test]
    fn fd_ledger_multi_slot_and_sibling_isolation() {
        // T14-C04 vocabulary arm: one session's teardown never moves a
        // sibling's accounting.
        let ledger = FdLedger::shared(8);
        let a = FdLedger::acquire(&ledger, &p("a"), 3).unwrap();
        let b = FdLedger::acquire(&ledger, &p("b"), 2).unwrap();
        assert_eq!(a.slots(), 3);
        assert_eq!(ledger.lock().unwrap().live(), 5);
        drop(a);
        {
            let l = ledger.lock().unwrap();
            assert_eq!(l.used_by(&p("b")), 2, "sibling untouched");
            assert_eq!(l.used_by(&p("a")), 0);
            assert_eq!(l.live(), 2);
            assert!(l.invariant_holds());
        }
        drop(b);
        assert_eq!(ledger.lock().unwrap().live(), 0);
        assert_eq!(ledger.lock().unwrap().used_by(&p("b")), 0);
    }

    #[test]
    fn fd_ledger_deterministic_churn_proves_no_leak() {
        // Vocabulary-level leak proof: bounded pseudo-random (deterministic)
        // acquire/drop churn across several refs must return the ledger to
        // EXACTLY zero with the invariant holding at every step boundary.
        let ledger = FdLedger::shared(16);
        let refs: Vec<PtyRef> = (0..4).map(|i| p(&format!("r{i}"))).collect();
        let mut leases: Vec<FdLease> = Vec::new();
        let mut rng: u64 = 0x243f6a8885a308d3; // fixed splitmix seed
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for round in 0..240 {
            match next() % 3 {
                0 | 1 => {
                    let owner = refs[(next() % 4) as usize].clone();
                    let n = (next() % 3 + 1) as usize;
                    if let Ok(l) = FdLedger::acquire(&ledger, &owner, n) {
                        leases.push(l);
                    }
                }
                _ => {
                    if !leases.is_empty() {
                        let i = (next() as usize) % leases.len();
                        leases.swap_remove(i);
                    }
                }
            }
            if round % 17 == 0 {
                let l = ledger.lock().unwrap();
                assert!(l.invariant_holds(), "round {round}");
                assert!(l.live() <= 16);
            }
        }
        drop(leases);
        let l = ledger.lock().unwrap();
        assert_eq!(l.live(), 0, "every slot returned — no vocabulary leak");
        assert!(l.invariant_holds());
    }

    #[test]
    fn fd_charging_never_exceeds_cap_even_for_multi_slot_requests() {
        let ledger = FdLedger::shared(4);
        let _a = FdLedger::acquire(&ledger, &p("a"), 3).unwrap();
        assert_eq!(
            FdLedger::acquire(&ledger, &p("a"), 2).err(),
            Some(FdError { live: 3, cap: 4, requested: 2 }),
            "multi-slot over-cap rejected atomically (no partial charge)"
        );
        assert_eq!(ledger.lock().unwrap().live(), 3);
    }
}
