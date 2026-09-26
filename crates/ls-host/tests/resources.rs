//! T09 REAL-tier integration: resource hard-limit planning, admission and
//! metrics against the REAL host (never a mock of the host).
//!
//! Gate: `LSB_T09_REAL=1`. What runs REAL on THIS host class (measured
//! 2026-09-26, see tests/runtime/resources/EXPECTATIONS.md):
//! * C02 — storage hard-limit on a quota-less fs ⇒ REJECT BEFORE CREATE ✓
//! * C03 — effective cgroup values are READ from the real hierarchy ✓
//! * C04 — admission contention + cancellation conservation ✓ (threads)
//! * C05 — real memory observation: usage ≠ limit, window recorded ✓
//! * C01 — hard mem/PID enforcement + sibling isolation: BLOCKED (no cgroup
//!   delegation; the whole hierarchy is a read-only mount even for euid-0).
//!   The gated test SKIPS LOUDLY with the verbatim blocker evidence — the
//!   criterion is recorded, not weakened.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ls_core::admission::{
    AdmitDecision, AdmissionPolicy, Allocation, NodeAdmission, NodeBudget,
};
use ls_core::domain::resources::{ResourceRequest, MIB};
use ls_host::resources::{
    apply_cgroup_spec, cpu_millis_to_quota_us, detect_disk_quota, observe_memory,
    parse_cpu_max, plan_enforcement, quota_us_to_cpu_millis, CgroupSpec, CpuSpec,
    DiskPlan, PlanRejection, UsageSource, DEFAULT_PERIOD_US,
};

fn real_gate(tag: &str) -> Option<()> {
    if std::env::var("LSB_T09_REAL").ok().as_deref() != Some("1") {
        eprintln!("SKIP {tag}: set LSB_T09_REAL=1 to run the real host-tier slice");
        return None;
    }
    Some(())
}

fn real_mounts() -> String {
    fs::read_to_string("/proc/mounts").expect("real /proc/mounts")
}

// --------------------------------------------------------------- T09-C02

/// REAL C02: a required disk hard limit on a quota-less filesystem must be
/// REJECTED BEFORE CREATE — the plan API returns Err, so no caller can
/// proceed with a silent warning instead.
#[test]
fn t09_c02_real_storage_hard_limit_rejected_before_create() {
    let Some(()) = real_gate("T09-C02 real") else { return };
    let mounts = real_mounts();
    // /tmp is overlayfs on this host class (measured): no quota options.
    let disk = detect_disk_quota(Path::new("/tmp/ls-t09-c02"), &mounts);
    let view = ls_host::resources::probe_cgroup2(
        Path::new("/sys/fs/cgroup"),
        &mounts,
        ".ls-t09-c02-probe",
    );
    eprintln!(
        "T09-C02 evidence: disk={disk:?} writable={} evidence={}",
        view.writable, view.writable_evidence
    );
    match &disk {
        DiskPlan::Unsupported { evidence } => {
            assert!(evidence.contains("quota"), "evidence names the cause: {evidence}")
        }
        other => panic!(
            "this host's /tmp must have no quota mechanism (measured premise): {other:?}"
        ),
    }
    let req = ResourceRequest::unconstrained()
        .with_storage_bytes(1024 * MIB)
        .unwrap();
    let outcome = plan_enforcement(&req, &view, disk, DEFAULT_PERIOD_US);
    let PlanRejection { reasons } = outcome
        .expect_err("required storage hard limit on a quota-less fs MUST reject before create");
    assert_eq!(reasons.len(), 1);
    assert_eq!(reasons[0].field, "storage_bytes");
    assert!(
        reasons[0].evidence.contains("quota"),
        "the reason is the truth, not a generic failure: {}",
        reasons[0].evidence
    );
    eprintln!("T09-C02 reject evidence: {reasons:?}");
}

/// REAL C02 sibling law: requesting NOTHING is admitted with NOTHING
/// promised — rejection is scoped to the unsupported demands, never blanket.
#[test]
fn t09_c02_real_unconstrained_request_plans_empty_not_rejected() {
    let Some(()) = real_gate("T09-C02 real empty") else { return };
    let mounts = real_mounts();
    let view = ls_host::resources::probe_cgroup2(
        Path::new("/sys/fs/cgroup"),
        &mounts,
        ".ls-t09-c02b-probe",
    );
    let plan = plan_enforcement(
        &ResourceRequest::unconstrained(),
        &view,
        DiskPlan::Unlimited,
        DEFAULT_PERIOD_US,
    )
    .expect("no demand ⇒ no rejection");
    assert_eq!(plan.spec, CgroupSpec::default());
}

// --------------------------------------------------------------- T09-C03

/// REAL C03: the effective cpu.max of the REAL hierarchy is read and parsed;
/// unit conversion roundtrips against the REAL period. The value came from
/// the kernel file — two independent reads agree, and the shape is real.
#[test]
fn t09_c03_real_effective_cpu_quota_readback() {
    let Some(()) = real_gate("T09-C03 real") else { return };
    let p = PathBuf::from("/sys/fs/cgroup/cpu.max");
    let raw1 = fs::read_to_string(&p).expect("real cpu.max readable");
    let raw2 = fs::read_to_string(&p).expect("second read");
    assert_eq!(raw1, raw2, "kernel file read is stable, not synthesized");
    let (quota, period) = parse_cpu_max(raw1.trim()).expect("real cpu.max parses");
    assert!(period >= 1000, "cgroup2 legal period lower bound: {period}");
    eprintln!("T09-C03 evidence: cpu.max={raw1:?} → quota={quota:?} period={period}");
    // conversion roundtrip expressed in the REAL period units
    for millis in [100u64, 500, 1000, 2000] {
        let q = cpu_millis_to_quota_us(millis, period).unwrap();
        assert_eq!(
            quota_us_to_cpu_millis(q, period).unwrap(),
            millis,
            "millis→quota→millis is exact at the REAL period"
        );
    }
    // the parsed real value itself is a legal knob (or the real 'max')
    if let Some(q) = quota {
        assert!(q > 0, "real quota is a positive µs value");
        let _ = fs::read_to_string("/sys/fs/cgroup/memory.max")
            .map(|m| eprintln!("T09-C03 evidence: memory.max={m:?}"));
    } else {
        eprintln!("T09-C03 evidence: root is 'max' (unlimited) — a real parse arm, not a default");
    }
}

// --------------------------------------------------------------- T09-C04

/// REAL C04: concurrent admission with cancellation — the node never
/// oversells, cancelled tickets are skipped, and at full drain every
/// uncancelled request was materialized EXACTLY once (no leaked reservation,
/// no lost ticket, no permanent starvation).
#[test]
fn t09_c04_real_contention_cancel_and_drain_conserve() {
    let Some(()) = real_gate("T09-C04 real") else { return };
    let budget = NodeBudget {
        cpu_millis: 8000,
        memory_bytes: 8 * MIB,
        pids: 8,
        storage_bytes: 32 * MIB,
    };
    let unit = ResourceRequest::unconstrained()
        .with_cpu_millis(1000)
        .unwrap()
        .with_memory_bytes(MIB)
        .unwrap()
        .with_pids(1)
        .unwrap()
        .with_storage_bytes(4 * MIB)
        .unwrap();
    let gate = Arc::new(NodeAdmission::new(budget, AdmissionPolicy { queue_capacity: 4096 }));
    let mut joins = Vec::new();
    for _ in 0..8 {
        let g = Arc::clone(&gate);
        joins.push(std::thread::spawn(move || {
            let mut admitted: Vec<Allocation> = Vec::new();
            let mut tickets = Vec::new();
            for _ in 0..24 {
                match g.try_admit(unit) {
                    AdmitDecision::Admitted(a) => admitted.push(a),
                    AdmitDecision::Queued(t) => tickets.push(t),
                    AdmitDecision::Rejected(e) => panic!("no rejection possible here: {e}"),
                }
            }
            (admitted, tickets)
        }));
    }
    let mut live: Vec<Allocation> = Vec::new();
    let mut queued_n = 0usize;
    let mut cancelled_n = 0usize;
    for j in joins {
        let (admitted, tickets) = j.join().unwrap();
        live.extend(admitted);
        // cancel every second ticket — a REAL mixed cancel/contention stream
        for (i, t) in tickets.into_iter().enumerate() {
            if i % 2 == 0 {
                assert!(gate.cancel(t), "cancel precedes any release ⇒ valid");
                cancelled_n += 1;
            } else {
                queued_n += 1;
            }
        }
    }
    let certainly_admitted = live.len();
    assert!(certainly_admitted <= 8, "never oversold at 8×unit budget");
    let m = gate.metrics();
    assert_eq!(m.allocations_live, certainly_admitted);
    // drain through promotions; conservation accounting
    let mut materialized = certainly_admitted;
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
        materialized,
        certainly_admitted + queued_n,
        "every non-cancelled request materialized exactly once ({cancelled_n} cancelled were skipped)"
    );
    let done = gate.metrics();
    assert_eq!(done.queued, 0);
    assert_eq!(done.allocations_live, 0);
    assert_eq!(done.remaining, budget, "pristine after full drain");
    eprintln!(
        "T09-C04 evidence: admitted={certainly_admitted} promoted={} cancelled_skipped={cancelled_n}",
        materialized - certainly_admitted
    );
}

// --------------------------------------------------------------- T09-C05

/// REAL C05: observe a REAL busy process — capacity, configured limit and
/// sampled usage are three different statements; the window is recorded;
/// usage is never the limit in disguise.
#[test]
fn t09_c05_real_observation_separates_capacity_limit_usage() {
    let Some(()) = real_gate("T09-C05 real") else { return };
    let mut child = Command::new("/bin/dash")
        .arg("-c")
        .arg("while :; do :; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real dash busy-loop");
    let pid = child.id() as i32;
    let configured = Some(64 * MIB); // a CONFIGURED (not enforced) limit
    let window = Duration::from_millis(150);
    std::thread::sleep(window);
    let obs1 = observe_memory(Path::new("/proc"), Some(pid), configured, window)
        .expect("real /proc observation");
    std::thread::sleep(window);
    let obs2 = observe_memory(Path::new("/proc"), Some(pid), configured, window)
        .expect("second observation");
    child.kill().ok();
    child.wait().ok();
    assert!(obs1.capacity_bytes > 0, "real MemTotal anchor");
    assert_eq!(obs1.configured_limit_bytes, configured);
    assert_eq!(obs1.usage_source, UsageSource::ProcStatusVmHwm);
    let u1 = obs1.usage_bytes.expect("real VmHWM sample");
    let u2 = obs2.usage_bytes.expect("second sample");
    assert!(u1 > 0 && u2 > 0);
    assert!(u2 >= u1, "high-water mark is monotone by definition");
    assert!(
        Some(u1) != obs1.configured_limit_bytes,
        "usage is sampled truth, never the limit ({u1} vs {configured:?})"
    );
    assert_eq!(obs1.window_ms, 150, "sampling window recorded verbatim");
    assert!(obs2.sampled_at_unix_ms >= obs1.sampled_at_unix_ms);
    eprintln!(
        "T09-C05 evidence: capacity={}B limit={configured:?} usage {u1}B→{u2}B window=150ms",
        obs1.capacity_bytes
    );
}

// --------------------------------------------------------------- T09-C01

/// REAL C01 (BLOCKED on this host class): hard memory/PID enforcement needs
/// a writable delegated cgroup subtree. Measured blocker: the whole
/// hierarchy is a READ-ONLY mount (euid-0 mkdir fails EROFS; no 65534
/// delegation exists). The criterion stands unmodified — this test SKIPS
/// LOUDLY carrying the verbatim evidence instead of simulating enforcement.
#[test]
fn t09_c01_real_hard_limit_enforcement_blocked_with_evidence() {
    let Some(()) = real_gate("T09-C01 real") else { return };
    let mounts = real_mounts();
    let view = ls_host::resources::probe_cgroup2(
        Path::new("/sys/fs/cgroup"),
        &mounts,
        ".ls-t09-c01-probe",
    );
    if !view.writable {
        eprintln!(
            "BLOCKED T09-C01: no writable delegated cgroup subtree on this host.\n\
             evidence: mounted={} controllers={:?} subtree_control={:?}\n\
             writability: {}\n\
             criterion unchanged: hard mem/PID enforcement + sibling isolation needs \
             an authorized worker with cgroup delegation (see EXPECTATIONS.md)",
            view.mounted, view.controllers, view.subtree_control, view.writable_evidence
        );
        return;
    }
    // Authorized-worker arm: apply a real pids.max into a delegated child and
    // verify the read-back. NOT reached on this host (kept, not deleted):
    let child = Path::new("/sys/fs/cgroup").join("ls-t09-c01-scope");
    std::fs::create_dir(&child).expect("delegated subtree mkdir");
    let spec = CgroupSpec {
        cpu: Some(CpuSpec { quota_us: 50_000, period_us: 100_000 }),
        memory_max_bytes: Some(64 * MIB),
        pids_max: Some(32),
    };
    let applied = apply_cgroup_spec(&child, &spec).expect("real apply + verify");
    assert_eq!(applied.len(), 3);
    std::fs::remove_dir(&child).ok();
    eprintln!("T09-C01 evidence: applied+verified {applied:?}");
}
