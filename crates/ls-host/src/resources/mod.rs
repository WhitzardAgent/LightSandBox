//! Resource hard-limit planning, cgroup application and metrics honesty
//! (T09; docs/v5/09_RUNTIME_DETAILS.md §资源硬限,
//! docs/acceptance/04_PERFORMANCE.md §预算).
//!
//! Laws enforced here (each pinned by a test):
//! * A requested hard limit whose controller/quota is UNSUPPORTED on this
//!   host ⇒ the whole plan is REJECTED **before create**, with EVERY
//!   unsupportable field named plus its evidence (没有某控制器时不能静默丢
//!   字段; storage 无配额时拒绝而非后台轮询假装 hard limit).
//! * cgroup WRITES are verified by read-back (写读一致律): the value in the
//!   kernel file must equal the value we intended, else an error.
//! * Metrics never confuse capacity / configured limit / actual usage, and a
//!   usage that cannot be sampled is `None` + `Unavailable` — never the
//!   limit wearing a usage costume (不能拿 limit 伪装 usage).
//! * RLIMIT-style process guardrails are out of scope for naming: nothing
//!   here calls itself a cgroup hard limit unless it really is one
//!   (09_RUNTIME_DETAILS.md).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ls_core::domain::resources::ResourceRequest;

#[derive(Debug)]
pub enum ResourceError {
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The kernel file read back something other than what we wrote.
    Verify {
        path: PathBuf,
        wrote: String,
        read: String,
    },
    /// Unit conversion overflow / illegal knob values.
    Invalid(String),
    /// A mandatory honesty anchor (e.g. MemTotal) could not be read.
    MissingAnchor { path: PathBuf, detail: String },
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { op, path, source } => write!(f, "{op} {}: {source}", path.display()),
            Self::Verify { path, wrote, read } => write!(
                f,
                "cgroup write verify failed on {}: wrote {wrote:?} but read back {read:?}",
                path.display()
            ),
            Self::Invalid(detail) => write!(f, "invalid resource knob: {detail}"),
            Self::MissingAnchor { path, detail } => {
                write!(f, "missing metrics anchor {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for ResourceError {}

// ---------------------------------------------------------------- probe

/// Real (or fixture-rooted) view of the cgroup2 hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupView {
    /// cgroup.controllers was readable at the probed root.
    pub mounted: bool,
    pub controllers: Vec<String>,
    pub subtree_control: Vec<String>,
    /// We REALLY created+removed a probe directory at the root.
    pub writable: bool,
    /// Human-auditable evidence, e.g. the mkdir errno or the mounts options.
    pub writable_evidence: String,
    /// mount options for the cgroup2 mount (from the mounts table), verbatim.
    pub mount_options: String,
}

impl CgroupView {
    fn controller_supported(&self, name: &str) -> Result<(), String> {
        if !self.mounted {
            return Err(format!(
                "no cgroup2 hierarchy evidence at probed root (controller {name} unknowable)"
            ));
        }
        if !self.controllers.iter().any(|c| c == name) {
            return Err(format!(
                "controller {name} not in cgroup.controllers ({})",
                self.controllers.join(" ")
            ));
        }
        if !self.writable {
            return Err(format!(
                "controller {name} present but hierarchy is not writable by us ({})",
                self.writable_evidence
            ));
        }
        Ok(())
    }

    /// Per-controller support with the REAL reason bound to it.
    pub fn support(&self, name: &str) -> ControllerSupport {
        match self.controller_supported(name) {
            Ok(()) => ControllerSupport::Supported,
            Err(reason) => ControllerSupport::Unsupported { reason },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerSupport {
    Supported,
    Unsupported { reason: String },
}

/// Parse one mounts-table line for a given target path; returns its options.
fn mount_options_for(mounts: &str, target: &str) -> String {
    for line in mounts.lines() {
        let mut it = line.split_whitespace();
        let (_dev, mp, _fstype) = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        if mp == target {
            return it.next().unwrap_or("").to_string();
        }
    }
    String::new()
}

/// Probe a cgroup2 root. `mounts_table` is the verbatim /proc/mounts content
/// (injectable for fixtures); `probe_name` is the transient probe dir name.
pub fn probe_cgroup2(root: &Path, mounts_table: &str, probe_name: &str) -> CgroupView {
    let controllers = fs::read_to_string(root.join("cgroup.controllers"))
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let mounted = root.join("cgroup.controllers").is_file();
    let subtree_control = fs::read_to_string(root.join("cgroup.subtree_control"))
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let opts = mount_options_for(mounts_table, &root.to_string_lossy());
    // Writability is decided by TWO real signals, both recorded:
    // 1. the mount options (`ro` is authoritative — a root process may
    //    bypass directory permission bits, so a chmod fixture cannot fake
    //    this; the kernel enforces ro mounts for everyone);
    // 2. a REAL transient mkdir+rmdir probe (catches the rw-mount-but-not-
    //    delegated case for unprivileged identities).
    let mount_ro = opts.split(',').any(|o| o.trim() == "ro");
    let probe_dir = root.join(probe_name);
    let (probe_ok, probe_evidence) = match fs::create_dir(&probe_dir) {
        Ok(()) => match fs::remove_dir(&probe_dir) {
            Ok(()) => (true, "probe mkdir+rmdir ok".to_string()),
            Err(e) => (false, format!("probe rmdir failed: {e}")),
        },
        Err(e) => (
            false,
            format!(
                "probe mkdir failed: {} (os error {:?})",
                e,
                e.raw_os_error()
            ),
        ),
    };
    let writable = !mount_ro && probe_ok;
    let evidence = format!(
        "mount opts: {opts}{}; {probe_evidence}",
        if mount_ro { " (ro ⇒ not writable for anyone)" } else { "" }
    );
    CgroupView {
        mounted,
        controllers,
        subtree_control,
        writable,
        writable_evidence: evidence,
        mount_options: opts,
    }
}

// ------------------------------------------------------- unit conversion

pub const DEFAULT_PERIOD_US: u64 = 100_000;
pub const MAX_PERIOD_US: u64 = 1_000_000;

/// Canonical millis → CFS quota µs for a given period. Checked arithmetic;
/// rounds to the nearest µs (periods beyond 1 s are rejected: cgroup2 legal
/// range is 1 ms..1 s).
pub fn cpu_millis_to_quota_us(cpu_millis: u64, period_us: u64) -> Result<u64, ResourceError> {
    if !(1000..=MAX_PERIOD_US).contains(&period_us) {
        return Err(ResourceError::Invalid(format!(
            "period_us {period_us} outside cgroup2 legal range 1000..=1000000"
        )));
    }
    let scaled = cpu_millis.checked_mul(period_us).ok_or_else(|| {
        ResourceError::Invalid(format!("{cpu_millis}ms * {period_us}us overflows"))
    })?;
    Ok((scaled + 500) / 1000)
}

/// CFS quota µs → canonical millis (floor; the exact inverse for round
/// values — roundtrip law is pinned by test).
pub fn quota_us_to_cpu_millis(quota_us: u64, period_us: u64) -> Result<u64, ResourceError> {
    if period_us == 0 {
        return Err(ResourceError::Invalid("period_us must not be 0".into()));
    }
    let scaled = quota_us.checked_mul(1000).ok_or_else(|| {
        ResourceError::Invalid(format!("{quota_us}us * 1000 overflows"))
    })?;
    Ok(scaled / period_us)
}

/// Format a cpu.max line: `"<quota|max> <period>"`.
pub fn format_cpu_max(quota_us: Option<u64>, period_us: u64) -> String {
    match quota_us {
        Some(q) => format!("{q} {period_us}"),
        None => format!("max {period_us}"),
    }
}

/// Parse a cpu.max line; `max` → None (unlimited).
pub fn parse_cpu_max(line: &str) -> Result<(Option<u64>, u64), ResourceError> {
    let mut it = line.split_whitespace();
    let q = it.next().ok_or_else(|| ResourceError::Invalid("empty cpu.max".into()))?;
    let p = it
        .next()
        .ok_or_else(|| ResourceError::Invalid(format!("cpu.max missing period: {line:?}")))?
        .parse::<u64>()
        .map_err(|e| ResourceError::Invalid(format!("cpu.max period: {e}")))?;
    let quota = if q == "max" {
        None
    } else {
        Some(
            q.parse::<u64>()
                .map_err(|e| ResourceError::Invalid(format!("cpu.max quota: {e}")))?,
        )
    };
    Ok((quota, p))
}

// ---------------------------------------------------------- plan (C02)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CgroupSpec {
    pub cpu: Option<CpuSpec>,
    pub memory_max_bytes: Option<u64>,
    pub pids_max: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSpec {
    pub quota_us: u64,
    pub period_us: u64,
}

/// A field whose hard limit could not be promised, WITH its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectReason {
    pub field: &'static str,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskPlan {
    /// No storage_bytes requested.
    Unlimited,
    /// A hard storage limit WAS requested and a quota mechanism exists.
    ProjectQuota { mount_options: String },
    /// Requested but unsupported on this filesystem ⇒ plan MUST reject.
    Unsupported { evidence: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupPlan {
    pub spec: CgroupSpec,
    pub disk: DiskPlan,
}

/// The whole-create rejection: EVERY unsupportable field is named; nothing
/// is silently dropped or downgraded to a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRejection {
    pub reasons: Vec<RejectReason>,
}

impl std::fmt::Display for PlanRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "hard limits unsupported on this host:")?;
        for r in &self.reasons {
            write!(f, " [{}: {}]", r.field, r.evidence)?;
        }
        Ok(())
    }
}

impl std::error::Error for PlanRejection {}

/// Detect a disk-quota mechanism for `path` from a mounts table. v1 honest
/// detection: the covering mount must carry an explicit quota option —
/// overlay/tmpfs/unspecified ⇒ Unsupported (never pretend with du polling).
pub fn detect_disk_quota(path: &Path, mounts_table: &str) -> DiskPlan {
    let mut best: Option<(usize, &str, &str)> = None; // (prefix len, mp, line)
    for line in mounts_table.lines() {
        let mut it = line.split_whitespace();
        let (_dev, mp, _fs) = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        let mp_path = Path::new(mp);
        if path.starts_with(mp_path) {
            let len = mp.len();
            if best.map_or(true, |(l, _, _)| len > l) {
                best = Some((len, mp, line));
            }
        }
    }
    match best {
        None => DiskPlan::Unsupported {
            evidence: format!("no mount entry covers {}", path.display()),
        },
        Some((_, mp, line)) => {
            let opts = line.split_whitespace().nth(3).unwrap_or("");
            const QUOTA_OPTS: [&str; 3] = ["prjquota", "usrquota", "grpquota"];
            let quota = opts
                .split(',')
                .any(|o| QUOTA_OPTS.contains(&o.trim()));
            if quota {
                DiskPlan::ProjectQuota {
                    mount_options: format!("{mp} opts={opts}"),
                }
            } else {
                DiskPlan::Unsupported {
                    evidence: format!(
                        "covering mount {mp} has no project/user/group quota option (opts: {opts})"
                    ),
                }
            }
        }
    }
}

/// The pre-create gate: plan every requested hard limit against MEASURED
/// support. Any unsupported present field ⇒ reject the WHOLE plan with all
/// reasons (C02: 创建前拒绝，不能仅记录警告).
pub fn plan_enforcement(
    request: &ResourceRequest,
    view: &CgroupView,
    disk: DiskPlan,
    period_us: u64,
) -> Result<CgroupPlan, PlanRejection> {
    let mut reasons = Vec::new();
    let mut spec = CgroupSpec::default();

    if let Some(millis) = request.cpu_millis {
        match view.support("cpu") {
            ControllerSupport::Supported => {
                spec.cpu = Some(CpuSpec {
                    quota_us: cpu_millis_to_quota_us(millis, period_us)
                        .map_err(|e| PlanRejection {
                            reasons: vec![RejectReason {
                                field: "cpu_millis",
                                evidence: e.to_string(),
                            }],
                        })?,
                    period_us,
                });
            }
            ControllerSupport::Unsupported { reason } => reasons.push(RejectReason {
                field: "cpu_millis",
                evidence: reason,
            }),
        }
    }
    if let Some(bytes) = request.memory_bytes {
        match view.support("memory") {
            ControllerSupport::Supported => spec.memory_max_bytes = Some(bytes),
            ControllerSupport::Unsupported { reason } => reasons.push(RejectReason {
                field: "memory_bytes",
                evidence: reason,
            }),
        }
    }
    if let Some(pids) = request.pids {
        match view.support("pids") {
            ControllerSupport::Supported => spec.pids_max = Some(pids),
            ControllerSupport::Unsupported { reason } => reasons.push(RejectReason {
                field: "pids",
                evidence: reason,
            }),
        }
    }
    let disk = if request.storage_bytes.is_some() {
        match disk {
            DiskPlan::Unlimited => {
                // caller asked about a path that matched nothing meaningful
                DiskPlan::Unsupported {
                    evidence: "no quota probe was performed for the path".to_string(),
                }
            }
            other => other,
        }
    } else {
        DiskPlan::Unlimited
    };
    if request.storage_bytes.is_some() {
        if let DiskPlan::Unsupported { evidence } = &disk {
            reasons.push(RejectReason {
                field: "storage_bytes",
                evidence: evidence.clone(),
            });
        }
    }
    if reasons.is_empty() {
        Ok(CgroupPlan { spec, disk })
    } else {
        Err(PlanRejection { reasons })
    }
}

// ------------------------------------------------------------- writer

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntry {
    pub file: String,
    pub value: String,
}

/// Apply a CgroupSpec into an EXISTING cgroup dir (mkdir is the privilege
/// boundary and belongs to the caller). Every write is read back and
/// compared — a write the kernel did not take is an error, not a hope.
pub fn apply_cgroup_spec(cg_dir: &Path, spec: &CgroupSpec) -> Result<Vec<AppliedEntry>, ResourceError> {
    let mut applied = Vec::new();
    if let Some(cpu) = spec.cpu {
        let value = format_cpu_max(Some(cpu.quota_us), cpu.period_us);
        write_verified(cg_dir, "cpu.max", &value, &mut applied)?;
    }
    if let Some(mem) = spec.memory_max_bytes {
        write_verified(cg_dir, "memory.max", &mem.to_string(), &mut applied)?;
    }
    if let Some(pids) = spec.pids_max {
        write_verified(cg_dir, "pids.max", &pids.to_string(), &mut applied)?;
    }
    Ok(applied)
}

fn write_verified(
    cg_dir: &Path,
    file: &str,
    value: &str,
    applied: &mut Vec<AppliedEntry>,
) -> Result<(), ResourceError> {
    let path = cg_dir.join(file);
    fs::write(&path, format!("{value}\n")).map_err(|e| ResourceError::Io {
        op: "write",
        path: path.clone(),
        source: e,
    })?;
    verify_written(&path, value)?;
    applied.push(AppliedEntry {
        file: file.to_string(),
        value: value.to_string(),
    });
    Ok(())
}

/// The read-back comparison, factored out so the mismatch arm is testable
/// without privileged tricks.
fn verify_written(path: &Path, expected: &str) -> Result<(), ResourceError> {
    let read = fs::read_to_string(path).map_err(|e| ResourceError::Io {
        op: "read-back",
        path: path.to_path_buf(),
        source: e,
    })?;
    let trimmed = read.trim();
    if trimmed != expected {
        return Err(ResourceError::Verify {
            path: path.to_path_buf(),
            wrote: expected.to_string(),
            read: trimmed.to_string(),
        });
    }
    Ok(())
}

// ------------------------------------------------------------- metrics

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    /// /proc/<pid>/status VmHWM (physical high-water mark — never virtual
    /// address space; ASAN-style address clamps are not physical billing).
    ProcStatusVmHwm,
    Unavailable,
}

/// One honest observation: capacity, configured limit and actual usage are
/// distinct fields; an unsampleable usage is None + Unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryObservation {
    pub capacity_bytes: u64,
    pub configured_limit_bytes: Option<u64>,
    pub usage_bytes: Option<u64>,
    pub usage_source: UsageSource,
    pub window_ms: u64,
    pub sampled_at_unix_ms: u64,
}

fn parse_kib_field(content: &str, field: &str) -> Option<u64> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let kib: u64 = rest
                .trim_start_matches(':')
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return kib.checked_mul(1024);
        }
    }
    None
}

/// Observe memory honesty for a sandbox process. `proc_root` is injectable
/// (fixture trees in unit tests; the REAL /proc in the gated tier).
pub fn observe_memory(
    proc_root: &Path,
    pid: Option<i32>,
    configured_limit_bytes: Option<u64>,
    window: Duration,
) -> Result<MemoryObservation, ResourceError> {
    let meminfo_path = proc_root.join("meminfo");
    let meminfo = fs::read_to_string(&meminfo_path).map_err(|e| ResourceError::Io {
        op: "read",
        path: meminfo_path.clone(),
        source: e,
    })?;
    let capacity = parse_kib_field(&meminfo, "MemTotal").ok_or_else(|| {
        ResourceError::MissingAnchor {
            path: meminfo_path,
            detail: "MemTotal absent — capacity cannot be stated".into(),
        }
    })?;
    let (usage_bytes, usage_source) = match pid {
        Some(p) => {
            let status_path = proc_root.join(p.to_string()).join("status");
            match fs::read_to_string(&status_path) {
                Ok(status) => match parse_kib_field(&status, "VmHWM") {
                    Some(v) => (Some(v), UsageSource::ProcStatusVmHwm),
                    None => (None, UsageSource::Unavailable),
                },
                Err(_) => (None, UsageSource::Unavailable),
            }
        }
        None => (None, UsageSource::Unavailable),
    };
    let sampled_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(MemoryObservation {
        capacity_bytes: capacity,
        configured_limit_bytes,
        usage_bytes,
        usage_source,
        window_ms: window.as_millis() as u64,
        sampled_at_unix_ms: sampled_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ls-t09-res-{}-{}",
            std::process::id(),
            tag
        ));
        if d.exists() {
            fs::remove_dir_all(&d).unwrap();
        }
        fs::create_dir_all(&d).unwrap();
        d
    }

    const MOUNTS_RO: &str =
        "cgroup /sys/fs/cgroup cgroup2 ro,nosuid,nodev,noexec,relatime 0 0\n";
    const MOUNTS_RW: &str = "cgroup /sys/fs/cgroup cgroup2 rw,nosuid,nodev,noexec,relatime 0 0\n";

    #[test]
    fn cpu_max_format_parse_roundtrip() {
        assert_eq!(format_cpu_max(Some(50_000), DEFAULT_PERIOD_US), "50000 100000");
        let (q, p) = parse_cpu_max("50000 100000").unwrap();
        assert_eq!(q, Some(50_000));
        assert_eq!(p, 100_000);
        let (q, _) = parse_cpu_max("max 100000").unwrap();
        assert_eq!(q, None);
        assert_eq!(format_cpu_max(None, 100_000), "max 100000");
        assert!(parse_cpu_max("garbage").is_err());
        assert!(parse_cpu_max("50000").is_err());
    }

    #[test]
    fn cpu_unit_conversion_is_checked_and_roundtrips() {
        // 500m at the default period == half the period: 50_000us
        let q = cpu_millis_to_quota_us(500, DEFAULT_PERIOD_US).unwrap();
        assert_eq!(q, 50_000);
        assert_eq!(quota_us_to_cpu_millis(q, DEFAULT_PERIOD_US).unwrap(), 500);
        // exact multiples across periods roundtrip exactly
        for millis in [1u64, 250, 1000, 1750, 64_000] {
            let q = cpu_millis_to_quota_us(millis, DEFAULT_PERIOD_US).unwrap();
            assert_eq!(quota_us_to_cpu_millis(q, DEFAULT_PERIOD_US).unwrap(), millis);
        }
        // illegal periods rejected, never guessed
        assert!(cpu_millis_to_quota_us(500, 0).is_err());
        assert!(cpu_millis_to_quota_us(500, 2_000_000).is_err());
        // overflow is an error, not a wrap
        assert!(cpu_millis_to_quota_us(u64::MAX, DEFAULT_PERIOD_US).is_err());
        assert!(quota_us_to_cpu_millis(u64::MAX, 1).is_err(), "u64::MAX µs * 1000 overflows — error, never a wrap");
    }

    #[test]
    fn probe_distinguishes_writable_from_read_only() {
        let d = tmpdir("probe-rw");
        fs::write(d.join("cgroup.controllers"), "cpuset cpu io memory pids\n").unwrap();
        fs::write(d.join("cgroup.subtree_control"), "").unwrap();
        let mounts = MOUNTS_RW.replace("/sys/fs/cgroup", &d.display().to_string());
        let view = probe_cgroup2(&d, &mounts, ".probe-rw");
        assert!(view.mounted);
        assert!(view.writable, "{:?}", view.writable_evidence);
        assert!(matches!(view.support("memory"), ControllerSupport::Supported));
        assert!(matches!(view.support("devices"), ControllerSupport::Unsupported { .. }));
        // read-only hierarchy: the mounts TABLE says `ro` — authoritative
        // even for root (kernel-enforced; a chmod fixture would be bypassed
        // by euid-0 and is deliberately NOT used)
        let d2 = tmpdir("probe-ro");
        fs::write(d2.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        let mounts2 = MOUNTS_RO.replace("/sys/fs/cgroup", &d2.display().to_string());
        let view2 = probe_cgroup2(&d2, &mounts2, ".probe-ro");
        assert!(view2.mounted);
        assert!(view2.mount_options.contains("ro"));
        assert!(!view2.writable, "ro mount must not be writable: {}", view2.writable_evidence);
        let reason = match view2.support("memory") {
            ControllerSupport::Unsupported { reason } => reason,
            other => panic!("memory must be unsupported on ro hierarchy: {other:?}"),
        };
        assert!(reason.contains("not writable"), "{reason}");
        assert!(view2.writable_evidence.contains("ro"), "evidence binds the ro mount option");
        fs::remove_dir_all(&d).ok();
        fs::remove_dir_all(&d2).ok();
    }

    #[test]
    fn disk_quota_detection_never_pretends() {
        let overlay = "overlay /tmp overlay rw,relatime,lowerdir=/a,upperdir=/b,workdir=/c 0 0\n\
                       cgroup /sys/fs/cgroup cgroup2 ro,relatime 0 0\n";
        match detect_disk_quota(Path::new("/tmp/ls-x"), overlay) {
            DiskPlan::Unsupported { evidence } => {
                assert!(evidence.contains("no project/user/group quota"), "{evidence}")
            }
            other => panic!("overlay without quota opts must be unsupported: {other:?}"),
        }
        let with_prj = "overlay / overlay rw,relatime 0 0\n\
                        /dev/sda /data ext4 rw,relatime,prjquota 0 0\n";
        // longest-prefix wins: /data is the covering mount for /data/x
        match detect_disk_quota(Path::new("/data/x"), with_prj) {
            DiskPlan::ProjectQuota { mount_options } => {
                assert!(mount_options.contains("prjquota"), "{mount_options}")
            }
            other => panic!("prjquota mount must be detected: {other:?}"),
        }
        match detect_disk_quota(Path::new("/elsewhere"), with_prj) {
            DiskPlan::Unsupported { .. } => {}
            other => panic!("non-quota covering mount: {other:?}"),
        }
    }

    #[test]
    fn plan_rejects_every_unsupported_field_before_create() {
        let d = tmpdir("plan-ro");
        fs::write(d.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        let mounts = MOUNTS_RO.replace("/sys/fs/cgroup", &d.display().to_string());
        let view = probe_cgroup2(&d, &mounts, ".probe-plan");
        let req = ResourceRequest::unconstrained()
            .with_cpu_millis(500)
            .unwrap()
            .with_memory_bytes(1024)
            .unwrap()
            .with_pids(16)
            .unwrap()
            .with_storage_bytes(4096)
            .unwrap();
        let disk = detect_disk_quota(Path::new("/tmp/x"), "overlay /tmp overlay rw,relatime 0 0\n");
        let err = plan_enforcement(&req, &view, disk, DEFAULT_PERIOD_US)
            .expect_err("RO hierarchy + no quota ⇒ whole plan rejects");
        let fields: Vec<_> = err.reasons.iter().map(|r| r.field).collect();
        assert_eq!(
            fields,
            vec!["cpu_millis", "memory_bytes", "pids", "storage_bytes"],
            "EVERY unsupported field is named — nothing silently dropped"
        );
        assert!(err.to_string().contains("storage_bytes"));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn plan_enforces_exact_values_when_supported() {
        let d = tmpdir("plan-ok");
        fs::write(d.join("cgroup.controllers"), "cpu memory pids\n").unwrap();
        let mounts = MOUNTS_RW.replace("/sys/fs/cgroup", &d.display().to_string());
        let view = probe_cgroup2(&d, &mounts, ".probe-plan2");
        let req = ResourceRequest::unconstrained()
            .with_cpu_millis(500)
            .unwrap()
            .with_memory_bytes(64 * 1024 * 1024)
            .unwrap()
            .with_pids(32)
            .unwrap();
        let plan = plan_enforcement(&req, &view, DiskPlan::Unlimited, DEFAULT_PERIOD_US)
            .expect("supported plan");
        assert_eq!(plan.spec.cpu, Some(CpuSpec { quota_us: 50_000, period_us: 100_000 }));
        assert_eq!(plan.spec.memory_max_bytes, Some(64 * 1024 * 1024));
        assert_eq!(plan.spec.pids_max, Some(32));
        assert_eq!(plan.disk, DiskPlan::Unlimited);
        // empty request ⇒ nothing promised, nothing rejected
        let empty = plan_enforcement(
            &ResourceRequest::unconstrained(),
            &view,
            DiskPlan::Unlimited,
            DEFAULT_PERIOD_US,
        )
        .unwrap();
        assert_eq!(empty.spec, CgroupSpec::default());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn writer_applies_and_verifies_every_line() {
        let d = tmpdir("writer");
        for f in ["cpu.max", "memory.max", "pids.max"] {
            fs::write(d.join(f), "").unwrap();
        }
        let spec = CgroupSpec {
            cpu: Some(CpuSpec { quota_us: 50_000, period_us: 100_000 }),
            memory_max_bytes: Some(64 * 1024 * 1024),
            pids_max: Some(32),
        };
        let applied = apply_cgroup_spec(&d, &spec).unwrap();
        assert_eq!(applied.len(), 3);
        assert_eq!(fs::read_to_string(d.join("cpu.max")).unwrap().trim(), "50000 100000");
        assert_eq!(
            fs::read_to_string(d.join("memory.max")).unwrap().trim(),
            (64 * 1024 * 1024).to_string()
        );
        // verify-mismatch arm: honest error, never panic
        let err = verify_written(&d.join("memory.max"), "999").unwrap_err();
        match err {
            ResourceError::Verify { wrote, read, .. } => {
                assert_eq!(wrote, "999");
                assert_eq!(read, (64 * 1024 * 1024).to_string());
            }
            other => panic!("{other:?}"),
        }
        // write into a NONEXISTENT cgroup dir ⇒ structured Io error (ENOENT
        // is root-proof; a chmod fixture would be bypassed by euid-0)
        let ghost = d.join("no-such-cgroup");
        let err2 = apply_cgroup_spec(&ghost, &spec).unwrap_err();
        assert!(matches!(err2, ResourceError::Io { op: "write", .. }), "{err2}");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn metrics_three_quantities_are_three_fields() {
        let d = tmpdir("metrics");
        fs::write(d.join("meminfo"), "MemTotal:       1024 kB\nMemFree: 1 kB\n").unwrap();
        fs::create_dir(d.join("4242")).unwrap();
        fs::write(
            d.join("4242").join("status"),
            "Name:\tdash\nVmHWM:\t     256 kB\n",
        )
        .unwrap();
        let obs = observe_memory(
            &d,
            Some(4242),
            Some(1_048_576),
            Duration::from_millis(250),
        )
        .unwrap();
        assert_eq!(obs.capacity_bytes, 1024 * 1024);
        assert_eq!(obs.configured_limit_bytes, Some(1_048_576));
        assert_eq!(obs.usage_bytes, Some(256 * 1024));
        assert_ne!(
            obs.usage_bytes, obs.configured_limit_bytes,
            "usage is a sampled truth, structurally distinct from the limit"
        );
        assert_eq!(obs.usage_source, UsageSource::ProcStatusVmHwm);
        assert_eq!(obs.window_ms, 250);
        assert!(obs.sampled_at_unix_ms > 0);
        // unsampleable pid ⇒ None + Unavailable, NEVER the limit
        let obs2 = observe_memory(&d, Some(9999), Some(1_048_576), Duration::from_millis(1)).unwrap();
        assert_eq!(obs2.usage_bytes, None);
        assert_eq!(obs2.usage_source, UsageSource::Unavailable);
        let obs3 = observe_memory(&d, None, Some(1_048_576), Duration::from_millis(1)).unwrap();
        assert_eq!(obs3.usage_bytes, None);
        fs::remove_dir_all(&d).ok();
    }
}
