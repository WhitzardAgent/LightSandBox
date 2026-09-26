//! Node capacity admission: bounded budgets, FIFO queueing with backpressure,
//! reservations and release (T09; docs/acceptance/04_PERFORMANCE.md §预算,
//! docs/v5/09_RUNTIME_DETAILS.md §资源硬限).
//!
//! Vocabulary (canonical units from `crate::domain::resources`):
//! * `NodeBudget` — the node's hard capacities (never oversold);
//! * `Allocation` — a HELD reservation; releasing CONSUMES it, so a double
//!   release is unexpressible (ownership law, not a runtime check);
//! * `Ticket` — a queued request; cancelling CONSUMES it (取消已入队);
//! * `Rejected` — backpressure: queue full, or a request that can NEVER fit
//!   (exceeds the total budget) — that one is rejected immediately instead of
//!   being parked into guaranteed starvation (无永久饥饿 guard).
//!
//! Fairness law: while the queue is non-empty, EVERY new request queues
//! behind it (a stream of small requests may never snatch released capacity
//! ahead of a big head — no technical starvation), promotions happen in
//! strict FIFO order on release, and an impossible request NEVER enters the
//! queue. Billing is PHYSICAL (memory_bytes) — an ASAN-style virtual-address
//! clamp is not a physical-memory budget and is never billed as one (T09
//! review focus).

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::domain::error::DomainError;
use crate::domain::resources::ResourceRequest;

/// The node's hard capacities. A dimension of `0` means THAT dimension
/// admits nothing (a present request against it is impossible); dimensions
/// are all explicit — a budget is a measured statement, never "unlimited".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeBudget {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub pids: u64,
    pub storage_bytes: u64,
}

impl NodeBudget {
    pub fn as_request(&self) -> ResourceRequest {
        ResourceRequest {
            memory_bytes: Some(self.memory_bytes),
            cpu_millis: Some(self.cpu_millis),
            pids: Some(self.pids),
            storage_bytes: Some(self.storage_bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionPolicy {
    /// Maximum QUEUED (waiting) requests; beyond this the caller gets
    /// backpressure (`QueueFull`), never an unbounded park.
    pub queue_capacity: usize,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self { queue_capacity: 64 }
    }
}

/// A HELD reservation. Not Clone on purpose: releasing consumes it, so one
/// reservation can be released exactly once by construction.
#[derive(Debug, PartialEq, Eq)]
pub struct Allocation {
    pub id: u64,
    pub request: ResourceRequest,
}

/// A queued request. Not Clone: cancelling consumes it.
#[derive(Debug, PartialEq, Eq)]
pub struct Ticket {
    pub seq: u64,
    pub request: ResourceRequest,
}

/// Which budget dimension made a request impossible / is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDimension {
    CpuMillis,
    MemoryBytes,
    Pids,
    StorageBytes,
}

impl BudgetDimension {
    pub fn name(self) -> &'static str {
        match self {
            Self::CpuMillis => "cpu_millis",
            Self::MemoryBytes => "memory_bytes",
            Self::Pids => "pids",
            Self::StorageBytes => "storage_bytes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    /// The request can NEVER fit this node (exceeds the TOTAL budget even
    /// when fully free) — rejected at the door, never starved in the queue.
    Impossible {
        dimension: BudgetDimension,
        requested: u64,
        budget: u64,
    },
    /// Bounded queue is full: backpressure to the caller.
    QueueFull { capacity: usize },
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Impossible {
                dimension,
                requested,
                budget,
            } => write!(
                f,
                "request can never fit: {}={} exceeds total node budget {}",
                dimension.name(),
                requested,
                budget
            ),
            Self::QueueFull { capacity } => {
                write!(f, "admission queue full (capacity {capacity}) — backpressure")
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

impl From<AdmissionError> for DomainError {
    fn from(e: AdmissionError) -> Self {
        use crate::domain::error::ErrorCode;
        match e {
            // unsatisfiable as stated against THIS node — a bad request, not
            // a transient shortage
            AdmissionError::Impossible { .. } => DomainError::invalid(e.to_string()),
            // transient backpressure — retryable later
            AdmissionError::QueueFull { .. } => {
                DomainError::new(ErrorCode::ResourceExhausted, e.to_string())
            }
        }
    }
}

/// The three possible outcomes of a non-blocking admit attempt: the caller
/// always learns the TRUTH about where its request went.
#[derive(Debug)]
pub enum AdmitDecision {
    Admitted(Allocation),
    Queued(Ticket),
    Rejected(AdmissionError),
}

/// Result of a release: promotions from the queue that fit into the freed
/// capacity, in strict FIFO order.
#[derive(Debug)]
pub struct ReleaseOutcome {
    pub promoted: Vec<Allocation>,
}

/// Point-in-time ledger view (metrics honesty: HELD and REMAINING are
/// distinct statements; queue length is reported separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionMetrics {
    pub budget: NodeBudget,
    pub held: NodeBudget,
    pub remaining: NodeBudget,
    pub queued: usize,
    pub allocations_live: usize,
}

struct Ledger {
    held_cpu: u64,
    held_mem: u64,
    held_pids: u64,
    held_storage: u64,
    live: usize,
    queue: VecDeque<(u64, ResourceRequest)>,
    next_id: u64,
}

impl Ledger {
    fn remaining(&self, budget: &NodeBudget) -> NodeBudget {
        NodeBudget {
            cpu_millis: budget.cpu_millis - self.held_cpu,
            memory_bytes: budget.memory_bytes - self.held_mem,
            pids: budget.pids - self.held_pids,
            storage_bytes: budget.storage_bytes - self.held_storage,
        }
    }

    fn fits(
        req: &ResourceRequest,
        remaining: &NodeBudget,
    ) -> Result<(), BudgetDimension> {
        if let Some(v) = req.cpu_millis {
            if v > remaining.cpu_millis {
                return Err(BudgetDimension::CpuMillis);
            }
        }
        if let Some(v) = req.memory_bytes {
            if v > remaining.memory_bytes {
                return Err(BudgetDimension::MemoryBytes);
            }
        }
        if let Some(v) = req.pids {
            if v > remaining.pids {
                return Err(BudgetDimension::Pids);
            }
        }
        if let Some(v) = req.storage_bytes {
            if v > remaining.storage_bytes {
                return Err(BudgetDimension::StorageBytes);
            }
        }
        Ok(())
    }

    /// First dimension of the TOTAL budget the request exceeds, if any.
    fn impossible(req: &ResourceRequest, budget: &NodeBudget) -> Option<AdmissionError> {
        if let Some(v) = req.cpu_millis {
            if v > budget.cpu_millis {
                return Some(AdmissionError::Impossible {
                    dimension: BudgetDimension::CpuMillis,
                    requested: v,
                    budget: budget.cpu_millis,
                });
            }
        }
        if let Some(v) = req.memory_bytes {
            if v > budget.memory_bytes {
                return Some(AdmissionError::Impossible {
                    dimension: BudgetDimension::MemoryBytes,
                    requested: v,
                    budget: budget.memory_bytes,
                });
            }
        }
        if let Some(v) = req.pids {
            if v > budget.pids {
                return Some(AdmissionError::Impossible {
                    dimension: BudgetDimension::Pids,
                    requested: v,
                    budget: budget.pids,
                });
            }
        }
        if let Some(v) = req.storage_bytes {
            if v > budget.storage_bytes {
                return Some(AdmissionError::Impossible {
                    dimension: BudgetDimension::StorageBytes,
                    requested: v,
                    budget: budget.storage_bytes,
                });
            }
        }
        None
    }

    fn hold(&mut self, req: &ResourceRequest) {
        self.held_cpu += req.cpu_millis.unwrap_or(0);
        self.held_mem += req.memory_bytes.unwrap_or(0);
        self.held_pids += req.pids.unwrap_or(0);
        self.held_storage += req.storage_bytes.unwrap_or(0);
        self.live += 1;
    }

    fn unhold(&mut self, req: &ResourceRequest) {
        self.held_cpu -= req.cpu_millis.unwrap_or(0);
        self.held_mem -= req.memory_bytes.unwrap_or(0);
        self.held_pids -= req.pids.unwrap_or(0);
        self.held_storage -= req.storage_bytes.unwrap_or(0);
        self.live -= 1;
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// The atomic admission gate: every decision is taken under ONE lock, so the
/// ledger can never go negative (no oversell) regardless of thread
/// contention. All operations are non-blocking; waiting is the caller's
/// explicit choice (背压, not hidden sleep).
pub struct NodeAdmission {
    budget: NodeBudget,
    policy: AdmissionPolicy,
    ledger: Mutex<Ledger>,
}

impl NodeAdmission {
    pub fn new(budget: NodeBudget, policy: AdmissionPolicy) -> Self {
        Self {
            budget,
            policy,
            ledger: Mutex::new(Ledger {
                held_cpu: 0,
                held_mem: 0,
                held_pids: 0,
                held_storage: 0,
                live: 0,
                queue: VecDeque::new(),
                next_id: 0,
            }),
        }
    }

    /// Try to admit `request`: fit-now → held Allocation; else bounded queue
    /// → Ticket; impossible or queue-full → Rejected with the true reason.
    pub fn try_admit(&self, request: ResourceRequest) -> AdmitDecision {
        let mut ledger = self.ledger.lock().unwrap();
        if let Some(err) = Ledger::impossible(&request, &self.budget) {
            return AdmitDecision::Rejected(err);
        }
        // Anti-starvation law (04_PERFORMANCE.md 公平调度): while the queue
        // is non-empty, EVERY new request queues behind it — otherwise a
        // stream of small requests could perpetually snatch released
        // capacity ahead of a big head (technical starvation). Idle capacity
        // is the accepted price of the guarantee.
        let remaining = ledger.remaining(&self.budget);
        if ledger.queue.is_empty() && Ledger::fits(&request, &remaining).is_ok() {
            ledger.hold(&request);
            let id = ledger.take_id();
            return AdmitDecision::Admitted(Allocation { id, request });
        }
        if ledger.queue.len() >= self.policy.queue_capacity {
            return AdmitDecision::Rejected(AdmissionError::QueueFull {
                capacity: self.policy.queue_capacity,
            });
        }
        // ticket seqs share the id counter with allocations — monotonic
        // across both, so FIFO order is a single comparable number.
        let seq = ledger.take_id();
        ledger.queue.push_back((seq, request));
        AdmitDecision::Queued(Ticket { seq, request })
    }

    /// Release a held allocation (consumes it). Freed capacity is
    /// immediately offered to the queue in strict FIFO order: the head is
    /// promoted when it fits; a misfitting head BLOCKS later promotions (no
    /// jump-ahead), because every queued request was verified POSSIBLE at
    /// entry — later releases will fit it (no permanent starvation).
    pub fn release(&self, allocation: Allocation) -> ReleaseOutcome {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.unhold(&allocation.request);
        let mut promoted = Vec::new();
        loop {
            let Some((_, head)) = ledger.queue.front() else { break };
            let remaining = ledger.remaining(&self.budget);
            if Ledger::fits(head, &remaining).is_err() {
                break; // strict FIFO: head-of-line blocks, never jumps
            }
            let (_, req) = ledger.queue.pop_front().unwrap();
            ledger.hold(&req);
            let id = ledger.take_id();
            promoted.push(Allocation { id, request: req });
        }
        ReleaseOutcome { promoted }
    }

    /// Cancel a QUEUED request (consumes the ticket). Returns false when the
    /// seq is unknown — e.g. already promoted: cancelling an ADMITTED
    /// allocation is impossible here (取消已接受不是取消已完成, 06_HTTP).
    pub fn cancel(&self, ticket: Ticket) -> bool {
        let mut ledger = self.ledger.lock().unwrap();
        let Some(pos) = ledger.queue.iter().position(|(seq, _)| *seq == ticket.seq)
        else {
            return false;
        };
        ledger.queue.remove(pos);
        true
    }

    pub fn metrics(&self) -> AdmissionMetrics {
        let ledger = self.ledger.lock().unwrap();
        AdmissionMetrics {
            budget: self.budget,
            held: NodeBudget {
                cpu_millis: ledger.held_cpu,
                memory_bytes: ledger.held_mem,
                pids: ledger.held_pids,
                storage_bytes: ledger.held_storage,
            },
            remaining: ledger.remaining(&self.budget),
            queued: ledger.queue.len(),
            allocations_live: ledger.live,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::resources::MIB;
    use std::sync::Arc;

    fn budget() -> NodeBudget {
        NodeBudget {
            cpu_millis: 4000,
            memory_bytes: 4 * MIB,
            pids: 4,
            storage_bytes: 16 * MIB,
        }
    }

    fn unit() -> ResourceRequest {
        ResourceRequest::unconstrained()
            .with_cpu_millis(1000)
            .unwrap()
            .with_memory_bytes(MIB)
            .unwrap()
            .with_pids(1)
            .unwrap()
            .with_storage_bytes(4 * MIB)
            .unwrap()
    }

    #[test]
    fn admit_to_capacity_then_exact_boundary() {
        // unconstrained on an EMPTY gate admits immediately
        let fresh = NodeAdmission::new(budget(), AdmissionPolicy::default());
        match fresh.try_admit(ResourceRequest::unconstrained()) {
            AdmitDecision::Admitted(_) => {}
            other => panic!("unconstrained on empty gate admits, got {other:?}"),
        }
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        for _ in 0..4 {
            match gate.try_admit(unit()) {
                AdmitDecision::Admitted(_) => {}
                other => panic!("capacity admits, got {other:?}"),
            }
        }
        // fifth: every dimension exhausted → queued, not rejected
        match gate.try_admit(unit()) {
            AdmitDecision::Queued(_) => {}
            other => panic!("exhausted-but-possible must queue, got {other:?}"),
        }
        // uniform fairness law: while ANYONE is queued, even a zero-demand
        // request queues behind — the gate has one comparable FIFO order
        match gate.try_admit(ResourceRequest::unconstrained()) {
            AdmitDecision::Queued(_) => {}
            other => panic!("queue non-empty ⇒ unconstrained queues too, got {other:?}"),
        }
    }

    #[test]
    fn release_restores_exact_capacity() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        let alloc = match gate.try_admit(unit()) {
            AdmitDecision::Admitted(a) => a,
            other => panic!("{other:?}"),
        };
        let before = gate.metrics();
        assert_eq!(before.remaining.memory_bytes, 3 * MIB);
        gate.release(alloc);
        let after = gate.metrics();
        assert_eq!(after.held, zero_budget());
        assert_eq!(after.remaining, budget(), "release returns capacity exactly");
        assert_eq!(after.allocations_live, 0);
    }

    #[test]
    fn fifo_promotion_order_on_release() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        let held: Vec<_> = (0..4)
            .map(|_| match gate.try_admit(unit()) {
                AdmitDecision::Admitted(a) => a,
                other => panic!("{other:?}"),
            })
            .collect();
        let t1 = match gate.try_admit(unit()) {
            AdmitDecision::Queued(t) => t,
            other => panic!("{other:?}"),
        };
        let t2 = match gate.try_admit(unit()) {
            AdmitDecision::Queued(t) => t,
            other => panic!("{other:?}"),
        };
        assert!(t2.seq > t1.seq, "seqs mint in arrival order");
        // release one slot → exactly t1's request is promoted (one fits)
        let out = gate.release(held.into_iter().next().unwrap());
        assert_eq!(out.promoted.len(), 1);
        assert_eq!(out.promoted[0].request, t1.request, "strict FIFO: t1 first");
        // the second release promotes t2's — never out of order
        let m = gate.metrics();
        assert_eq!(m.queued, 1, "t2 still queued");
    }

    #[test]
    fn cancelled_ticket_is_skipped_and_promoted_seq_cannot_cancel() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        let held: Vec<_> = (0..4)
            .map(|_| match gate.try_admit(unit()) {
                AdmitDecision::Admitted(a) => a,
                other => panic!("{other:?}"),
            })
            .collect();
        let t1 = match gate.try_admit(unit()) {
            AdmitDecision::Queued(t) => t,
            other => panic!("{other:?}"),
        };
        let t2 = match gate.try_admit(unit()) {
            AdmitDecision::Queued(t) => t,
            other => panic!("{other:?}"),
        };
        let t1_seq = t1.seq;
        assert!(gate.cancel(t1), "queued ticket cancels");
        assert_eq!(gate.metrics().queued, 1, "cancel frees the slot");
        // promotion now skips the cancelled request entirely
        let out = gate.release(held.into_iter().next().unwrap());
        assert_eq!(out.promoted.len(), 1);
        assert_eq!(
            out.promoted[0].request, t2.request,
            "cancelled head is skipped; the next REAL ticket is served"
        );
        // the promoted seq is no longer cancellable (取消已接受不是取消已完成)
        assert!(!gate.cancel(t2));
        // releasing the promoted allocation works and restores capacity
        let out2 = gate.release(out.promoted.into_iter().next().unwrap());
        assert!(out2.promoted.is_empty());
        let _ = t1_seq;
    }

    #[test]
    fn queue_full_is_backpressure_not_a_park() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy { queue_capacity: 1 });
        let held: Vec<_> = (0..4)
            .map(|_| match gate.try_admit(unit()) {
                AdmitDecision::Admitted(a) => a,
                other => panic!("{other:?}"),
            })
            .collect();
        let _t = match gate.try_admit(unit()) {
            AdmitDecision::Queued(t) => t,
            other => panic!("{other:?}"),
        };
        match gate.try_admit(unit()) {
            AdmitDecision::Rejected(AdmissionError::QueueFull { capacity }) => {
                assert_eq!(capacity, 1)
            }
            other => panic!("second waiter = backpressure, got {other:?}"),
        }
        drop(held);
    }

    #[test]
    fn impossible_request_is_rejected_at_the_door_not_starved() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        // larger than the TOTAL pids budget though everything is free
        let greedy = ResourceRequest::unconstrained().with_pids(5).unwrap();
        match gate.try_admit(greedy) {
            AdmitDecision::Rejected(AdmissionError::Impossible {
                dimension,
                requested,
                budget: b,
            }) => {
                assert_eq!(dimension, BudgetDimension::Pids);
                assert_eq!(requested, 5);
                assert_eq!(b, 4);
            }
            other => panic!("impossible must reject immediately, got {other:?}"),
        }
        assert_eq!(gate.metrics().queued, 0, "impossible NEVER parks in the queue");
        // memory dimension names itself
        let fat = ResourceRequest::unconstrained()
            .with_memory_bytes(5 * MIB)
            .unwrap();
        match gate.try_admit(fat) {
            AdmitDecision::Rejected(AdmissionError::Impossible { dimension, .. }) => {
                assert_eq!(dimension.name(), "memory_bytes")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn strict_fifo_head_of_line_blocks_without_jump_ahead() {
        // 2 free units; head needs 3 (possible: budget 4), later needs 1.
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        let two = ResourceRequest::unconstrained()
            .with_memory_bytes(2 * MIB)
            .unwrap();
        let held: Vec<_> = (0..2)
            .map(|_| match gate.try_admit(two) {
                AdmitDecision::Admitted(a) => a,
                other => panic!("{other:?}"),
            })
            .collect();
        let big = ResourceRequest::unconstrained()
            .with_memory_bytes(3 * MIB)
            .unwrap();
        let small = ResourceRequest::unconstrained()
            .with_memory_bytes(MIB)
            .unwrap();
        let _t_big = match gate.try_admit(big) {
            AdmitDecision::Queued(t) => t,
            other => panic!("big queues: {other:?}"),
        };
        // anti-starvation law: while ANYONE is queued, even a request that
        // would fit the CURRENT remainder must queue behind — no snatching
        // released capacity ahead of the big head, ever.
        let t_small = match gate.try_admit(small) {
            AdmitDecision::Queued(t) => t,
            other => panic!("queue non-empty ⇒ small queues too: {other:?}"),
        };
        assert!(t_small.seq > _t_big.seq);
        // first release frees 2 units: head big still does not fit, and the
        // small behind it is NOT jumped (strict FIFO at promotion either)
        let (h1, h2) = {
            let mut it = held.into_iter();
            (it.next().unwrap(), it.next().unwrap())
        };
        let out = gate.release(h1);
        assert!(out.promoted.is_empty(), "head does not fit ⇒ nothing promotes");
        // second release returns the node to 4 free: big promotes FIRST…
        let out2 = gate.release(h2);
        assert_eq!(out2.promoted.len(), 2, "both fit once capacity returns");
        assert_eq!(out2.promoted[0].request, big, "FIFO: big before small");
        assert_eq!(out2.promoted[1].request, small);
    }

    #[test]
    fn never_oversold_and_no_ticket_lost_under_thread_contention() {
        const REQUESTS_PER_THREAD: usize = 8;
        let gate = Arc::new(NodeAdmission::new(budget(), AdmissionPolicy { queue_capacity: 1024 }));
        let mut joins = Vec::new();
        for _ in 0..8 {
            let g = Arc::clone(&gate);
            joins.push(std::thread::spawn(move || {
                let mut admitted = Vec::new();
                for _ in 0..REQUESTS_PER_THREAD {
                    if let AdmitDecision::Admitted(a) = g.try_admit(unit()) {
                        admitted.push(a);
                    }
                }
                admitted
            }));
        }
        let mut live: Vec<_> = joins.into_iter().flat_map(|j| j.join().unwrap()).collect();
        let m = gate.metrics();
        assert_eq!(live.len(), m.allocations_live);
        // oversell law: held never exceeds budget in ANY dimension
        assert!(m.held.cpu_millis <= m.budget.cpu_millis);
        assert!(m.held.memory_bytes <= m.budget.memory_bytes);
        assert!(m.held.pids <= m.budget.pids);
        assert!(m.held.storage_bytes <= m.budget.storage_bytes);
        // exact pack on this unit size is 4, never 5
        assert_eq!(live.len(), 4, "budget 4×unit admits exactly 4");
        // conservation law: drain releases through promotions until the node
        // is quiet; EVERY request is materialized exactly once — no leaked
        // reservation, no lost ticket (无泄漏预留).
        let total_requests = 8 * REQUESTS_PER_THREAD;
        let mut materialized = live.len();
        while !live.is_empty() {
            let mut next = Vec::new();
            for a in live {
                let out = gate.release(a);
                materialized += out.promoted.len();
                next.extend(out.promoted);
            }
            live = next;
        }
        assert_eq!(
            materialized, total_requests,
            "every queued request was promoted exactly once"
        );
        let m2 = gate.metrics();
        assert_eq!(m2.queued, 0);
        assert_eq!(m2.held, zero_budget());
        assert_eq!(m2.remaining, budget(), "pristine ledger after full drain");
        assert_eq!(m2.allocations_live, 0);
    }

    #[test]
    fn metrics_never_confuse_held_with_remaining() {
        let gate = NodeAdmission::new(budget(), AdmissionPolicy::default());
        let a = match gate.try_admit(unit()) {
            AdmitDecision::Admitted(a) => a,
            other => panic!("{other:?}"),
        };
        let m = gate.metrics();
        assert_eq!(m.held.memory_bytes, MIB, "HELD is the reservation");
        assert_eq!(m.remaining.memory_bytes, 3 * MIB, "REMAINING is distinct");
        assert_eq!(m.budget.memory_bytes, 4 * MIB);
        gate.release(a);
    }

    #[test]
    fn admission_error_display_names_the_truth() {
        let e = AdmissionError::Impossible {
            dimension: BudgetDimension::Pids,
            requested: 9,
            budget: 4,
        };
        let s = e.to_string();
        assert!(s.contains("pids") && s.contains('9') && s.contains('4'), "{s}");
        let q = AdmissionError::QueueFull { capacity: 1 }.to_string();
        assert!(q.contains("backpressure"), "{q}");
    }

    fn zero_budget() -> NodeBudget {
        NodeBudget {
            cpu_millis: 0,
            memory_bytes: 0,
            pids: 0,
            storage_bytes: 0,
        }
    }
}
