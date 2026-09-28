//! T13 — persistent-shell session state machine, PURE LOGIC layer
//! (plans/tasks/T13.md; docs/v5/03_DOMAIN.md Session row §资源与生命周期;
//! the wire mirror of these types lives in `ls_api::sessions`).
//!
//! This module owns session TRUTH; it never spawns or talks to a shell.
//! A real interpreter loop (supervisor/worker leg) pulls [`Dispatch`]
//! values, runs them in the long-lived shell, and reports back ONLY through
//! typed [`SessionControl`] frames. Laws:
//!
//! * PERSISTENCE BY THE SHELL, NOT BY RE-EXEC: one session == one
//!   long-lived shell. cwd/env changes persist because the SAME process
//!   keeps state (C01). The host learns post-command state exclusively via
//!   `CommandFinished.observed`, applied as a FULL replacement; an absent
//!   observation means "no observation reported", never "reconstruct from
//!   output" and never "assume unchanged semantics beyond our record".
//! * CONTROL NEVER COMES FROM OUTPUT BYTES: [`SessionManager::accept_output`]
//!   accounts producer bytes per stream and does not look at them. A
//!   payload containing serializations of every known control frame moves
//!   the byte counters and nothing else (C02). All state transitions come
//!   from typed frames routed by the transport's channel tag.
//! * SERIALIZATION IS EXPLICIT (C03): commands enter a FIFO queue in
//!   registration order (`register` assigns the seq); `dispatch` admits at
//!   most one active command and returns `None` while one runs. The
//!   concurrency policy is deterministic at the boundary:
//! [`ConcurrencyPolicy::Serialize`] (bounded queue; overflow is an explicit
//!   `Busy` rejection) or [`ConcurrencyPolicy::RejectWhileBusy`] (any second
//!   in-flight command is an explicit conflict — both honest answers to
//!   03_DOMAIN's "execute in registration order OR explicit conflict").
//! * HANDLE DIE WITH THE SHELL (C04): `ShellExited` aborts the active and
//!   every queued command in registration order and seals the record as
//!   `exited`; every later call on that id except `close` fails with
//!   [`SessionError::Exited`]. Ordering law: the worker posts `ShellExited`
//!   only after draining the pipes, so output arriving after it is a
//!   protocol error here.
//! * CLEANUP IS EXPLICIT AND IDs NEVER RECYCLE: `close` reclaims the record
//!   and burns the id (03_DOMAIN's deleted-ids-are-never-reused rule);
//!   re-opening a burned id is [`SessionError::BurnedId`]. Generation is
//!   fenced on every control frame (stale-generation frames are rejected
//!   before any state read).
//!
//! Worker-gated legs (real shell processes, cd/export round trips, Ctrl-C
//! on a foreground pipeline) are registered in
//! tests/runtime/sessions/EXPECTATIONS.md — none of them is faked here.

use crate::streams::StreamId;
use ls_core::domain::execution::ExitRecord;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Default depth of the per-session waiting queue.
pub const DEFAULT_QUEUE_CAP: usize = 64;
/// Scripts are bounded at the state-machine boundary too (defense in depth;
/// the wire layer enforces the same cap).
pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

// ------------------------------------------------------------------- types

/// Deterministic same-session concurrency policy (C03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyPolicy {
    /// FIFO in registration order with a bounded WAITING queue (the active
    /// command does not count against `max_queued`); overflow rejects the
    /// arriving command with `Busy`. `max_queued` must be >= 1.
    Serialize { max_queued: usize },
    /// At most one command in flight: any register while a command is
    /// active or waiting is an explicit `Busy` conflict.
    RejectWhileBusy,
}

impl Default for ConcurrencyPolicy {
    fn default() -> Self {
        ConcurrencyPolicy::Serialize {
            max_queued: DEFAULT_QUEUE_CAP,
        }
    }
}

/// One persistent shell's creation parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    /// Guest ABSOLUTE interpreter path (never host-resolved).
    pub shell: String,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

/// Post-command environment observation reported on the control channel.
/// Applied as a full replacement of the persisted view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedState {
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// Typed control frames — the ONLY legal source of session transitions.
/// Mirror of `ls_api::sessions::ControlFrame`, reusing the frozen
/// [`ExitRecord`] union (exited / signaled / unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControl {
    CommandStarted { seq: u64 },
    CommandFinished {
        seq: u64,
        exit: ExitRecord,
        observed: Option<ObservedState>,
    },
    ShellExited { exit: ExitRecord },
}

/// Why a pending command was aborted before/while running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortCause {
    ShellExited,
    Closed,
}

/// State changes produced by one accepted control frame, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    CommandStarted { seq: u64 },
    CommandSettled { seq: u64, exit: ExitRecord },
    /// The accompanying observation replaced the persisted cwd/env.
    ObservationApplied { seq: u64 },
    CommandAborted { seq: u64, cause: AbortCause },
    ShellDied { exit: ExitRecord },
}

/// Fail-closed session errors; each maps to the frozen error domain at the
/// façade (NotFound→NOT_FOUND, Exited/Closed/Busy→CONFLICT-family,
/// validation→INVALID_ARGUMENT, fencing→PRECONDITION_FAILED).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    NotFound,
    /// Id belonged to a closed session; ids are never recycled.
    BurnedId,
    AlreadyRegistered,
    /// Shell is dead; every handle on this session is invalid (C04).
    Exited,
    /// Concurrency policy rejected the arrival (queue full / busy).
    Busy,
    PolicyInvalid(&'static str),
    ScriptInvalid(&'static str),
    /// A lifecycle frame referenced a seq that is not the active command.
    ControlMismatch { active: Option<u64>, got: u64 },
    /// Frame arrived with a stale sandbox generation (fencing).
    StaleGeneration { recorded: u64, got: u64 },
    /// Merged-pty payload offered to a plain shell session.
    UnsupportedStream,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotFound => write!(f, "no such session"),
            SessionError::BurnedId => write!(f, "session id was closed and is never reused"),
            SessionError::AlreadyRegistered => write!(f, "session id already live"),
            SessionError::Exited => write!(f, "shell exited; session handles are invalid"),
            SessionError::Busy => write!(f, "session is busy (concurrency policy)"),
            SessionError::PolicyInvalid(why) => write!(f, "invalid concurrency policy: {why}"),
            SessionError::ScriptInvalid(why) => write!(f, "invalid script: {why}"),
            SessionError::ControlMismatch { active, got } => {
                write!(f, "control frame seq {got} does not match active {active:?}")
            }
            SessionError::StaleGeneration { recorded, got } => {
                write!(f, "stale generation {got} (recorded {recorded})")
            }
            SessionError::UnsupportedStream => write!(f, "stream label not served by shell sessions"),
        }
    }
}

impl std::error::Error for SessionError {}

/// A command admitted for execution: seq + verbatim script + the persisted
/// environment snapshot the shell observes at dispatch time (proving C01
/// persistence comes from the shell's own state, not from re-sending env).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub seq: u64,
    pub script: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// Read model for inspect/reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// "open" | "exited" ("closed" sessions have no record to view).
    pub state: &'static str,
    pub sandbox_id: String,
    pub generation: u64,
    pub active: Option<u64>,
    pub queued: Vec<u64>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub exit: Option<ExitRecord>,
}

/// Result of explicit cleanup; the record is gone afterwards and the id is
/// burned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseOutcome {
    /// "open" | "exited" — what the session was before reclamation.
    pub prior_state: &'static str,
    /// Seqs aborted by the close (active first, then queue FIFO).
    pub aborted: Vec<u64>,
}

// ----------------------------------------------------------------- session

struct ShellSession {
    sandbox_id: String,
    generation: u64,
    policy: ConcurrencyPolicy,
    state: State,
    cwd: String,
    env: BTreeMap<String, String>,
    next_seq: u64,
    queue: VecDeque<(u64, String)>,
    active: Option<(u64, String)>,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

enum State {
    Open,
    Exited { exit: ExitRecord },
}

// ----------------------------------------------------------------- manager

/// Owns every session on the node. Pure logic: no process, no io.
pub struct SessionManager {
    sessions: HashMap<String, ShellSession>,
    burned: HashSet<String>,
}

impl SessionManager {
    pub fn new() -> SessionManager {
        SessionManager {
            sessions: HashMap::new(),
            burned: HashSet::new(),
        }
    }

    /// The open/any-state gate shared by every op: unknown then dead.
    fn live(&mut self, id: &str) -> Result<&mut ShellSession, SessionError> {
        match self.sessions.get_mut(id) {
            None => Err(SessionError::NotFound),
            Some(s) => match s.state {
                State::Open => Ok(s),
                State::Exited { .. } => Err(SessionError::Exited),
            },
        }
    }

    /// Register a new persistent shell session. `policy` is validated
    /// before anything is recorded.
    pub fn open(
        &mut self,
        id: &str,
        sandbox_id: &str,
        generation: u64,
        spec: ShellSpec,
        policy: ConcurrencyPolicy,
    ) -> Result<(), SessionError> {
        if let ConcurrencyPolicy::Serialize { max_queued } = policy {
            if max_queued == 0 {
                return Err(SessionError::PolicyInvalid("max_queued must be >= 1"));
            }
        }
        if id.is_empty() || id.contains('\0') {
            return Err(SessionError::PolicyInvalid(
                "session id must be non-empty and NUL-free",
            ));
        }
        if self.burned.contains(id) {
            return Err(SessionError::BurnedId);
        }
        if self.sessions.contains_key(id) {
            return Err(SessionError::AlreadyRegistered);
        }
        self.sessions.insert(
            id.to_string(),
            ShellSession {
                sandbox_id: sandbox_id.to_string(),
                generation,
                policy,
                state: State::Open,
                cwd: spec.cwd.unwrap_or_else(|| "/".to_string()),
                env: spec.env,
                next_seq: 0,
                queue: VecDeque::new(),
                active: None,
                stdout_bytes: 0,
                stderr_bytes: 0,
            },
        );
        Ok(())
    }

    /// Enqueue a command in registration order; returns its session-local
    /// seq. The sec stays monotonic for the session's whole life.
    pub fn register(&mut self, id: &str, script: &str) -> Result<u64, SessionError> {
        let s = self.live(id)?;
        if script.is_empty() {
            return Err(SessionError::ScriptInvalid("empty"));
        }
        if script.len() > MAX_SCRIPT_BYTES {
            return Err(SessionError::ScriptInvalid("oversize"));
        }
        if script.contains('\0') {
            return Err(SessionError::ScriptInvalid("nul"));
        }
        let busy = match s.policy {
            ConcurrencyPolicy::Serialize { max_queued } => s.queue.len() >= max_queued,
            ConcurrencyPolicy::RejectWhileBusy => s.active.is_some() || !s.queue.is_empty(),
        };
        if busy {
            return Err(SessionError::Busy);
        }
        let seq = s.next_seq;
        s.next_seq += 1;
        s.queue.push_back((seq, script.to_string()));
        Ok(seq)
    }

    /// Admit the queue head as THE active command (at most one). `None`
    /// means "busy or nothing waiting" — never a guess.
    pub fn dispatch(&mut self, id: &str) -> Result<Option<Dispatch>, SessionError> {
        let s = self.live(id)?;
        if s.active.is_some() {
            return Ok(None);
        }
        let Some((seq, script)) = s.queue.pop_front() else {
            return Ok(None);
        };
        s.active = Some((seq, script.clone()));
        Ok(Some(Dispatch {
            seq,
            script,
            cwd: s.cwd.clone(),
            env: s.env.clone(),
        }))
    }

    /// Apply one typed control frame. Generation fencing runs before any
    /// state read; a rejected frame leaves the session bit-identical.
    pub fn apply_control(
        &mut self,
        id: &str,
        generation: u64,
        frame: SessionControl,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let s = self.sessions.get_mut(id).ok_or(SessionError::NotFound)?;
        if s.generation != generation {
            return Err(SessionError::StaleGeneration {
                recorded: s.generation,
                got: generation,
            });
        }
        if matches!(s.state, State::Exited { .. }) {
            return Err(SessionError::Exited);
        }
        match frame {
            SessionControl::CommandStarted { seq } => {
                let active = s.active.as_ref().map(|(a, _)| *a);
                if active != Some(seq) {
                    return Err(SessionError::ControlMismatch { active, got: seq });
                }
                Ok(vec![SessionEvent::CommandStarted { seq }])
            }
            SessionControl::CommandFinished {
                seq,
                exit,
                observed,
            } => {
                let active = s.active.as_ref().map(|(a, _)| *a);
                if active != Some(seq) {
                    return Err(SessionError::ControlMismatch { active, got: seq });
                }
                s.active = None;
                let mut events = vec![SessionEvent::CommandSettled { seq, exit }];
                if let Some(obs) = observed {
                    s.cwd = obs.cwd;
                    s.env = obs.env;
                    events.push(SessionEvent::ObservationApplied { seq });
                }
                Ok(events)
            }
            SessionControl::ShellExited { exit } => {
                let mut events = Vec::new();
                if let Some((seq, _)) = s.active.take() {
                    events.push(SessionEvent::CommandAborted {
                        seq,
                        cause: AbortCause::ShellExited,
                    });
                }
                while let Some((seq, _)) = s.queue.pop_front() {
                    events.push(SessionEvent::CommandAborted {
                        seq,
                        cause: AbortCause::ShellExited,
                    });
                }
                events.push(SessionEvent::ShellDied { exit });
                s.state = State::Exited { exit };
                Ok(events)
            }
        }
    }

    /// Account producer output bytes per stream. Bytes are NEVER inspected
    /// (C02); the returned value is the new stream total.
    pub fn accept_output(
        &mut self,
        id: &str,
        stream: StreamId,
        bytes: &[u8],
    ) -> Result<u64, SessionError> {
        let s = self.live(id)?;
        let n = bytes.len() as u64;
        match stream {
            StreamId::Stdout => {
                s.stdout_bytes += n;
                Ok(s.stdout_bytes)
            }
            StreamId::Stderr => {
                s.stderr_bytes += n;
                Ok(s.stderr_bytes)
            }
            StreamId::Pty => Err(SessionError::UnsupportedStream),
        }
    }

    pub fn inspect(&self, id: &str) -> Result<SessionView, SessionError> {
        let s = self.sessions.get(id).ok_or(SessionError::NotFound)?;
        Ok(SessionView {
            state: match s.state {
                State::Open => "open",
                State::Exited { .. } => "exited",
            },
            sandbox_id: s.sandbox_id.clone(),
            generation: s.generation,
            active: s.active.as_ref().map(|(a, _)| *a),
            queued: s.queue.iter().map(|(q, _)| *q).collect(),
            cwd: s.cwd.clone(),
            env: s.env.clone(),
            stdout_bytes: s.stdout_bytes,
            stderr_bytes: s.stderr_bytes,
            exit: match s.state {
                State::Open => None,
                State::Exited { exit } => Some(exit),
            },
        })
    }

    /// Explicit cleanup: aborts pending handles, reclaims the record, burns
    /// the id. Second close is `NotFound` (the id does not come back).
    pub fn close(&mut self, id: &str) -> Result<CloseOutcome, SessionError> {
        let s = self.sessions.remove(id).ok_or(SessionError::NotFound)?;
        let prior_state = match s.state {
            State::Open => "open",
            State::Exited { .. } => "exited",
        };
        let mut aborted = Vec::new();
        if let Some((seq, _)) = s.active {
            aborted.push(seq);
        }
        aborted.extend(s.queue.iter().map(|(q, _)| *q));
        self.burned.insert(id.to_string());
        Ok(CloseOutcome {
            prior_state,
            aborted,
        })
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use ls_core::domain::execution::ExitRecord;

    fn spec(cwd: Option<&str>, env: &[(&str, &str)]) -> ShellSpec {
        ShellSpec {
            shell: "/bin/sh".to_string(),
            cwd: cwd.map(str::to_string),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn open_one(mgr: &mut SessionManager) {
        mgr.open("s1", "sbx-1", 3, spec(Some("/"), &[("A", "0")]), Default::default())
            .unwrap();
    }

    #[test]
    fn open_defaults_and_register_rejects_bad_scripts() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.state, "open");
        assert_eq!(v.sandbox_id, "sbx-1");
        assert_eq!(v.generation, 3);
        assert_eq!(v.cwd, "/");
        assert_eq!(v.env.get("A").map(String::as_str), Some("0"));
        assert_eq!(mgr.register("s1", ""), Err(SessionError::ScriptInvalid("empty")));
        assert_eq!(
            mgr.register("s1", &"x".repeat(MAX_SCRIPT_BYTES + 1)),
            Err(SessionError::ScriptInvalid("oversize"))
        );
        assert_eq!(mgr.register("s1", "a\0b"), Err(SessionError::ScriptInvalid("nul")));
        assert_eq!(mgr.register("s1", "pwd"), Ok(0));
        assert_eq!(mgr.register("s1", "ls"), Ok(1), "seq monotonic per session");
        assert_eq!(mgr.register("nope", "pwd"), Err(SessionError::NotFound));
    }

    #[test]
    fn duplicate_open_and_policy_validation() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        assert_eq!(
            mgr.open("s1", "sbx-1", 3, spec(None, &[]), Default::default()),
            Err(SessionError::AlreadyRegistered)
        );
        assert_eq!(
            mgr.open(
                "s2",
                "sbx-1",
                3,
                spec(None, &[]),
                ConcurrencyPolicy::Serialize { max_queued: 0 }
            ),
            Err(SessionError::PolicyInvalid("max_queued must be >= 1"))
        );
    }

    #[test]
    fn commands_serialize_in_registration_order() {
        let mut mgr = SessionManager::new();
        mgr.open(
            "s1",
            "sbx-1",
            3,
            spec(Some("/"), &[("A", "0")]),
            ConcurrencyPolicy::Serialize { max_queued: 3 },
        )
        .unwrap();
        for (i, c) in ["first", "second", "third"].iter().enumerate() {
            assert_eq!(mgr.register("s1", c), Ok(i as u64));
        }
        let d0 = mgr.dispatch("s1").unwrap().unwrap();
        assert_eq!((d0.seq, d0.script.as_str()), (0, "first"));
        assert_eq!((d0.cwd.as_str(), d0.env.get("A").map(String::as_str)), ("/", Some("0")));
        assert_eq!(mgr.dispatch("s1").unwrap(), None, "at most one active");
        mgr.apply_control(
            "s1",
            3,
            SessionControl::CommandFinished {
                seq: 0,
                exit: ExitRecord::Exited { exit_code: 0 },
                observed: None,
            },
        )
        .unwrap();
        let d1 = mgr.dispatch("s1").unwrap().unwrap();
        assert_eq!((d1.seq, d1.script.as_str()), (1, "second"));
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.active, Some(1));
        assert_eq!(v.queued, vec![2], "third still waits its turn");
    }

    #[test]
    fn queue_cap_overflow_is_explicit_busy() {
        let mut mgr = SessionManager::new();
        mgr.open(
            "s1",
            "sbx-1",
            3,
            spec(None, &[]),
            ConcurrencyPolicy::Serialize { max_queued: 1 },
        )
        .unwrap();
        assert_eq!(mgr.register("s1", "a"), Ok(0));
        assert_eq!(mgr.register("s1", "b"), Err(SessionError::Busy), "one-deep queue full");
        assert!(mgr.dispatch("s1").unwrap().is_some());
        assert_eq!(mgr.register("s1", "b"), Ok(1), "active does not count against the queue cap");
        assert_eq!(mgr.register("s1", "c"), Err(SessionError::Busy));
    }

    #[test]
    fn reject_while_busy_policy_conflicts_explicitly() {
        let mut mgr = SessionManager::new();
        mgr.open("s1", "sbx-1", 3, spec(None, &[]), ConcurrencyPolicy::RejectWhileBusy)
            .unwrap();
        assert_eq!(mgr.register("s1", "a"), Ok(0));
        assert_eq!(mgr.register("s1", "b"), Err(SessionError::Busy), "waiting counts as busy");
        let d = mgr.dispatch("s1").unwrap().unwrap();
        assert_eq!(d.seq, 0);
        assert_eq!(mgr.register("s1", "b"), Err(SessionError::Busy), "active counts as busy");
        mgr.apply_control(
            "s1",
            3,
            SessionControl::CommandFinished {
                seq: 0,
                exit: ExitRecord::Exited { exit_code: 0 },
                observed: None,
            },
        )
        .unwrap();
        assert_eq!(mgr.register("s1", "b"), Ok(1), "idle again after settle");
    }

    #[test]
    fn cwd_env_persist_across_commands_via_observation() {
        // C01 unit arm: cd/export in command 0 persist into the snapshot of
        // command 1 — via a control-channel observation, never output scans.
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.register("s1", "cd /app && export A=1 B=2").unwrap();
        let d0 = mgr.dispatch("s1").unwrap().unwrap();
        assert_eq!((d0.cwd.as_str(), d0.env.get("A").map(String::as_str)), ("/", Some("0")));
        let events = mgr
            .apply_control(
                "s1",
                3,
                SessionControl::CommandFinished {
                    seq: 0,
                    exit: ExitRecord::Exited { exit_code: 0 },
                    observed: Some(ObservedState {
                        cwd: "/app".to_string(),
                        env: [("A", "1"), ("B", "2")]
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    }),
                },
            )
            .unwrap();
        assert_eq!(
            events,
            vec![
                SessionEvent::CommandSettled {
                    seq: 0,
                    exit: ExitRecord::Exited { exit_code: 0 }
                },
                SessionEvent::ObservationApplied { seq: 0 }
            ]
        );
        mgr.register("s1", "pwd").unwrap();
        let d1 = mgr.dispatch("s1").unwrap().unwrap();
        assert_eq!(d1.seq, 1);
        assert_eq!(d1.cwd, "/app", "post-cd truth persists");
        assert_eq!(d1.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(d1.env.get("B").map(String::as_str), Some("2"));
    }

    #[test]
    fn command_without_observation_changes_no_recorded_state() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.register("s1", "true").unwrap();
        mgr.dispatch("s1").unwrap();
        let events = mgr
            .apply_control(
                "s1",
                3,
                SessionControl::CommandFinished {
                    seq: 0,
                    exit: ExitRecord::Exited { exit_code: 7 },
                    observed: None,
                },
            )
            .unwrap();
        assert_eq!(
            events,
            vec![SessionEvent::CommandSettled {
                seq: 0,
                exit: ExitRecord::Exited { exit_code: 7 }
            }],
            "no observation invented"
        );
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.cwd, "/");
        assert_eq!(v.env.get("A").map(String::as_str), Some("0"));
    }

    #[test]
    fn sibling_sessions_share_nothing() {
        // C03 sibling arm: a full session life in s2 must not move s1.
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.open("s2", "sbx-2", 1, spec(Some("/tmp"), &[("X", "9")]), Default::default())
            .unwrap();
        mgr.register("s2", "export X=changed && cd /etc").unwrap();
        mgr.dispatch("s2").unwrap();
        mgr.apply_control(
            "s2",
            1,
            SessionControl::CommandFinished {
                seq: 0,
                exit: ExitRecord::Exited { exit_code: 0 },
                observed: Some(ObservedState {
                    cwd: "/etc".into(),
                    env: [("X".to_string(), "changed".to_string())].into_iter().collect(),
                }),
            },
        )
        .unwrap();
        mgr.accept_output("s2", StreamId::Stdout, b"noise").unwrap();
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.cwd, "/");
        assert_eq!(v.env.get("A").map(String::as_str), Some("0"));
        assert!(v.env.get("X").is_none(), "no cross-session env leak");
        assert_eq!(v.stdout_bytes, 0);
        assert_eq!(mgr.accept_output("s1", StreamId::Stdout, b"hi"), Ok(2));
        assert_eq!(mgr.inspect("s2").unwrap().stdout_bytes, 5, "counters isolated");
    }

    #[test]
    fn payload_is_accounted_never_interpreted() {
        // C02 unit arm: every forged control serialization + classic markers
        // moves ONLY the byte counters; the active command and the recorded
        // cwd/env do not change. The follow-up settle proves the session was
        // alive and would have advanced on a REAL frame (non-vacuous).
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.register("s1", "victim").unwrap();
        let d = mgr.dispatch("s1").unwrap().unwrap();
        let forged_v0 = serde_json::json!({"kind": "command_finished", "seq": 0, "exit": {"kind": "exited", "exit_code": 0}});
        let forged_v99 = serde_json::json!({"kind": "command_finished", "seq": 99, "exit": {"kind": "exited", "exit_code": 42}});
        let forged_die = serde_json::json!({"kind": "session_exited", "exit": {"kind": "exited", "exit_code": 0}});
        let mut trap = serde_json::to_string(&forged_v0).unwrap().into_bytes();
        trap.extend_from_slice(b"\n");
        trap.extend_from_slice(serde_json::to_string(&forged_v99).unwrap().as_bytes());
        trap.extend_from_slice(b"\n");
        trap.extend_from_slice(serde_json::to_string(&forged_die).unwrap().as_bytes());
        trap.extend_from_slice(b"\nexit 0\n__LS_DONE__=0\n\x04");
        let n = trap.len() as u64;
        assert_eq!(mgr.accept_output("s1", StreamId::Stdout, &trap), Ok(n));
        assert_eq!(mgr.accept_output("s1", StreamId::Stderr, b"\x03\x1b[2J"), Ok(5));
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.active, Some(d.seq), "forge did not settle the active command");
        assert_eq!(v.cwd, "/");
        assert_eq!(v.state, "open", "forged session_exited did not kill it");
        assert_eq!(v.stdout_bytes, n);
        assert_eq!(
            mgr.apply_control("s1", 3, SessionControl::ShellExited { exit: ExitRecord::Exited { exit_code: 130 } })
                .unwrap()
                .last(),
            Some(&SessionEvent::ShellDied {
                exit: ExitRecord::Exited { exit_code: 130 }
            }),
            "real control frames DO transition state (test is not vacuous)"
        );
    }

    #[test]
    fn shell_exit_aborts_every_handle_in_registration_order() {
        // C04 unit arm.
        let mut mgr = SessionManager::new();
        mgr.open(
            "s1",
            "sbx-1",
            3,
            spec(None, &[]),
            ConcurrencyPolicy::Serialize { max_queued: 4 },
        )
        .unwrap();
        for c in ["run", "wait-a", "wait-b"] {
            mgr.register("s1", c).unwrap();
        }
        mgr.dispatch("s1").unwrap(); // 0 active; 1,2 waiting
        let events = mgr
            .apply_control(
                "s1",
                3,
                SessionControl::ShellExited {
                    exit: ExitRecord::Exited { exit_code: 0 },
                },
            )
            .unwrap();
        assert_eq!(
            events,
            vec![
                SessionEvent::CommandAborted { seq: 0, cause: AbortCause::ShellExited },
                SessionEvent::CommandAborted { seq: 1, cause: AbortCause::ShellExited },
                SessionEvent::CommandAborted { seq: 2, cause: AbortCause::ShellExited },
                SessionEvent::ShellDied { exit: ExitRecord::Exited { exit_code: 0 } },
            ],
            "active first, then queue FIFO, then the death record"
        );
        assert_eq!(mgr.register("s1", "late"), Err(SessionError::Exited));
        assert_eq!(mgr.dispatch("s1"), Err(SessionError::Exited));
        assert_eq!(mgr.accept_output("s1", StreamId::Stdout, b"x"), Err(SessionError::Exited));
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.state, "exited");
        assert_eq!(v.exit, Some(ExitRecord::Exited { exit_code: 0 }));
        assert!(v.queued.is_empty() && v.active.is_none());
    }

    #[test]
    fn started_frame_requires_the_active_seq() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.register("s1", "cmd").unwrap();
        assert_eq!(
            mgr.apply_control("s1", 3, SessionControl::CommandStarted { seq: 0 }),
            Err(SessionError::ControlMismatch { active: None, got: 0 }),
            "started before dispatch is a protocol error"
        );
        mgr.dispatch("s1").unwrap();
        assert_eq!(
            mgr.apply_control("s1", 3, SessionControl::CommandStarted { seq: 9 }),
            Err(SessionError::ControlMismatch { active: Some(0), got: 9 })
        );
        assert_eq!(
            mgr.apply_control("s1", 3, SessionControl::CommandStarted { seq: 0 }).unwrap(),
            vec![SessionEvent::CommandStarted { seq: 0 }],
            "the matching started frame is acknowledged"
        );
        // finishing a non-active seq is likewise rejected, nothing moves:
        assert_eq!(
            mgr.apply_control(
                "s1",
                3,
                SessionControl::CommandFinished {
                    seq: 5,
                    exit: ExitRecord::Exited { exit_code: 0 },
                    observed: None
                }
            ),
            Err(SessionError::ControlMismatch { active: Some(0), got: 5 })
        );
        assert_eq!(mgr.inspect("s1").unwrap().active, Some(0));
    }

    #[test]
    fn generation_fencing_rejects_stale_frames_before_any_state_read() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        mgr.register("s1", "cmd").unwrap();
        mgr.dispatch("s1").unwrap();
        assert_eq!(
            mgr.apply_control(
                "s1",
                2, // sandbox died and was recreated; old channels must not drive new state
                SessionControl::ShellExited { exit: ExitRecord::Exited { exit_code: 0 } }
            ),
            Err(SessionError::StaleGeneration { recorded: 3, got: 2 })
        );
        let v = mgr.inspect("s1").unwrap();
        assert_eq!(v.state, "open");
        assert_eq!(v.active, Some(0), "stale frame moved nothing");
    }

    #[test]
    fn close_is_explicit_cleanup_and_burns_the_id() {
        let mut mgr = SessionManager::new();
        mgr.open("s1", "sbx-1", 3, spec(None, &[]), ConcurrencyPolicy::Serialize { max_queued: 4 })
            .unwrap();
        for c in ["one", "two", "three"] {
            mgr.register("s1", c).unwrap();
        }
        mgr.dispatch("s1").unwrap();
        let out = mgr.close("s1").unwrap();
        assert_eq!(out.prior_state, "open");
        assert_eq!(out.aborted, vec![0, 1, 2], "aborted in registration order");
        assert_eq!(mgr.inspect("s1"), Err(SessionError::NotFound), "record reclaimed");
        assert_eq!(
            mgr.open("s1", "sbx-1", 4, spec(None, &[]), Default::default()),
            Err(SessionError::BurnedId),
            "ids never recycle, even at a new generation"
        );
        assert_eq!(mgr.close("s1"), Err(SessionError::NotFound));
        // a session that died keeps its record until close:
        mgr.open("s2", "sbx-1", 3, spec(None, &[]), Default::default()).unwrap();
        mgr.apply_control(
            "s2",
            3,
            SessionControl::ShellExited { exit: ExitRecord::Signaled { signal: 9 } },
        )
        .unwrap();
        let out = mgr.close("s2").unwrap();
        assert_eq!(out.prior_state, "exited");
        assert!(out.aborted.is_empty());
        assert_eq!(mgr.inspect("s2"), Err(SessionError::NotFound));
    }

    #[test]
    fn pty_payload_label_is_rejected_for_shell_sessions() {
        let mut mgr = SessionManager::new();
        open_one(&mut mgr);
        assert_eq!(
            mgr.accept_output("s1", StreamId::Pty, b"x"),
            Err(SessionError::UnsupportedStream)
        );
        assert_eq!(mgr.inspect("s1").unwrap().stdout_bytes, 0);
    }
}
