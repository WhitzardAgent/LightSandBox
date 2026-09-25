//! Rootfs lease lifecycle + garbage collection (T07-C04).
//!
//! * A prepared rootfs exists only alongside its lease record (`lease.json`)
//!   inside its sandbox dir; work begins under an `.incomplete` marker that
//!   is cleared only when the lease commits.
//! * Normal teardown is `RootfsLease::release` — explicit, never Drop
//!   (Drop deliberately does NOT delete: a dropped-not-released lease must
//!   leave disk state for the sweep, so crashes cannot leak invisible
//!   trees... the opposite trade also exists; honesty: Drop does not log
//!   either in v1 — the incomplete/lease state on disk is the record).
//! * The GC sweep (round-03 MAJOR #2) owns TWO classes, and ALWAYS audits
//!   `/proc/self/mountinfo` ahead of any deletion (a mount leak is
//!   reported, never silently deleted):
//!   1. incomplete debris — `sandbox-*` dirs still carrying `.incomplete`,
//!      inactive, and older than the caller cutoff;
//!   2. committed-lease reconciliation — `sandbox-*` dirs carrying
//!      `lease.json` that are inactive, stale (record `created_ms` +
//!      cutoff), and whose recorded owner pid is DEAD (or whose record is
//!      unreadable). A LIVE owner keeps its tree: it may legitimately hold
//!      paths into it.
//!   `bases/` (shared per-digest verified bases) and anything not named
//!   `sandbox-*` is outside sweep scope by construction. Per-entry failures
//!   never abort a pass; they are enumerated in `GcReport::errors`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::images::error::ImageError;

use super::prepare::PrepareReport;
use super::{RootfsHandle, Result};

pub const INCOMPLETE_MARKER: &str = ".incomplete";
pub const LEASE_FILE: &str = "lease.json";

/// On-disk lease record (self-generated; JSON shape is internal, not a
/// frozen contract surface).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub sandbox_id: String,
    pub base_digest: String,
    pub driver: String,
    pub rootfs: PathBuf,
    pub owner_pid: u32,
    pub created_ms: u64,
}

/// A committed live lease. Obtain via `RootfsManager::prepare`.
pub struct RootfsLease {
    dir: PathBuf,
    handle: RootfsHandle,
    report: PrepareReport,
    record: LeaseRecord,
    released: bool,
}

impl RootfsLease {
    pub fn handle(&self) -> &RootfsHandle {
        &self.handle
    }

    pub fn report(&self) -> &PrepareReport {
        &self.report
    }

    pub fn record(&self) -> &LeaseRecord {
        &self.record
    }

    pub fn sandbox_dir(&self) -> &Path {
        &self.dir
    }

    /// Normal teardown. Refuses — and keeps everything — when a live mount
    /// reference exists under the sandbox dir (mount-leak honesty: an
    /// umount leak must surface loudly, never be silenced by rm -rf).
    pub fn release(mut self) -> Result<()> {
        let leaks = mount_leaks_under(&self.dir)?;
        if !leaks.is_empty() {
            return Err(ImageError::Io {
                detail: format!(
                    "refusing to release sandbox dir with live mount leaks {:?}: {}",
                    leaks,
                    self.dir.display()
                ),
            });
        }
        fs::remove_dir_all(&self.dir).map_err(ImageError::from)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for RootfsLease {
    fn drop(&mut self) {
        // Deliberately no disk mutation: unreleased leases are swept by the
        // GC path (their sandbox dir still carries a lease.json; operators
        // reconcile via sweep/active-ids), never by an implicit Drop that
        // could delete state someone still holds a path into.
    }
}

pub fn mark_incomplete(sandbox_dir: &Path) -> Result<()> {
    fs::write(sandbox_dir.join(INCOMPLETE_MARKER), b"in-progress\n").map_err(ImageError::from)
}

/// Publish `bytes` as `dir/lease.json` crash-atomically (round-03 MAJOR
/// #2): write a uniquely-named tmp file, sync it, then rename over the
/// final path. A crash mid-commit leaves either the old state or the new
/// one — never a torn record. Tmp residue is only possible across a hard
/// crash and is swept with the sandbox dir it lives in.
fn write_lease_atomic(dir: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let tmp = dir.join(format!(".lease.json.tmp-{}-{n}", std::process::id()));
    let outcome = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
        drop(f);
        fs::rename(&tmp, dir.join(LEASE_FILE))
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    outcome.map_err(ImageError::from)
}

/// Commit a lease: write lease.json, clear `.incomplete`, return the live
/// handle. Called once by `RootfsManager::prepare` after the copy driver
/// succeeds. Ordering is significant: the atomic lease.json publish lands
/// FIRST and `.incomplete` is cleared LAST, so a crash between the two
/// leaves a marker-classified dir (incomplete debris) — the fail-closed
/// state the sweep already owns.
pub fn commit(sandbox_dir: &Path, handle: RootfsHandle, report: PrepareReport) -> Result<RootfsLease> {
    let record = LeaseRecord {
        lease_id: handle.lease_id.clone(),
        sandbox_id: sandbox_dir
            .file_name()
            .map(|s| s.to_string_lossy().trim_start_matches("sandbox-").to_string())
            .unwrap_or_default(),
        base_digest: handle.base_digest.clone(),
        driver: handle.driver.name().to_string(),
        rootfs: handle.root.clone(),
        owner_pid: std::process::id(),
        created_ms: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    let json = serde_json::to_vec_pretty(&record)?;
    write_lease_atomic(sandbox_dir, &json)?;
    fs::remove_file(sandbox_dir.join(INCOMPLETE_MARKER)).map_err(ImageError::from)?;
    Ok(RootfsLease {
        dir: sandbox_dir.to_path_buf(),
        handle,
        report,
        record,
        released: false,
    })
}

/// Read a lease record back (diagnostics/GC support).
pub fn read_lease(sandbox_dir: &Path) -> Result<LeaseRecord> {
    let bytes = fs::read(sandbox_dir.join(LEASE_FILE)).map_err(ImageError::from)?;
    serde_json::from_slice(&bytes).map_err(|e| ImageError::LayoutInvalid {
        detail: format!("corrupt lease.json in {}: {e}", sandbox_dir.display()),
    })
}

/// Parse `/proc/self/mountinfo`-shaped text: return every mount point that
/// lies under `base` (or equals it). Field 5 is the mount point; optional
/// fields run to the ` - ` separator, then fstype/source/super-opts.
/// Mountinfo octal-escapes spaces (\040), tabs (\011), newlines (\012) and
/// backslashes (\134) in paths — unescape before comparing.
pub fn parse_mount_leaks(content: &str, base: &Path) -> Vec<String> {
    let base_s = base.to_string_lossy();
    let mut out = Vec::new();
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let mp = unescape_mountinfo(fields[4]);
        if mp == base_s.as_ref()
            || (mp.starts_with(base_s.as_ref())
                && mp.as_bytes().get(base_s.len()) == Some(&b'/'))
        {
            out.push(mp);
        }
    }
    out
}

fn unescape_mountinfo(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                if oct.chars().all(|c| c.is_digit(8)) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Live mount references under `path` from the real `/proc/self/mountinfo`.
pub fn mount_leaks_under(path: &Path) -> Result<Vec<String>> {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let content = fs::read_to_string("/proc/self/mountinfo").map_err(ImageError::from)?;
    Ok(parse_mount_leaks(&content, &canon))
}

/// GC outcome — every classification is enumerated, no silent discards.
#[derive(Debug, Default)]
pub struct GcReport {
    /// incomplete prepare debris removed this pass.
    pub removed: Vec<PathBuf>,
    /// committed lease trees reclaimed this pass (round-03 MAJOR #2):
    /// stale + id not active + owner pid dead + no live mount reference.
    pub reclaimed_committed: Vec<PathBuf>,
    pub kept_active: Vec<PathBuf>,
    pub kept_fresh: Vec<PathBuf>,
    /// committed leases kept ONLY because their owner pid is still alive.
    pub kept_owner_live: Vec<PathBuf>,
    pub refused_mount_leak: Vec<PathBuf>,
    /// per-entry failures collected without aborting the whole pass
    /// (round-03 #5a). The top-level `read_dir(data_root)` failure is
    /// still returned as `Err` — only ENTRY-scoped failures land here.
    pub errors: Vec<(PathBuf, String)>,
}

/// Sweep `data_root`. Two removal classes, everything else kept:
///
/// **Incomplete debris** — `sandbox-*` dirs that
///   (a) still carry `.incomplete` (prepared never committed), AND
///   (b) are NOT listed in `active_ids`, AND
///   (c) the marker is at least `cutoff` old — NOTE: `Duration::ZERO`
///       matches ANY age (force-clean path used by tests/operators); crash
///       debris protection for in-flight prepares REQUIRES a nonzero
///       cutoff in production callers, AND
///   (d) have no live mount reference underneath.
///
/// **Committed lease reconciliation** (round-03 MAJOR #2) — `sandbox-*`
/// dirs carrying `lease.json` (no marker) that
///   (a) are NOT listed in `active_ids`, AND
///   (b) are stale (record `created_ms + cutoff <= now`; cutoff ZERO =
///       force, matching the incomplete class), AND
///   (c) have a DEAD owner (`/proc/<owner_pid>` absent) or an unreadable
///       record — a live owner may legitimately hold paths into the tree;
///       a dead one cannot. Caveat: PID REUSE could masquerade as a live
///       owner; v1 accepts this (start-time matching is out of scope) and
///       relies on the staleness cutoff plus the mount audit as mitigations.
///   (d) have no live mount reference underneath.
///
/// Cutoff discipline (round-03 #5b): the `.incomplete` marker mtime is
/// stamped ONCE at prepare start and never refreshed while work proceeds,
/// so a production cutoff must be ≥ the LONGEST possible prepare —
/// otherwise a slow, healthy prepare is misclassified as debris. The same
/// rule applies to the committed class via `created_ms` (stamped once at
/// commit): a cutoff forces reclamation of any dead-owner lease older than
/// it, regardless of how recently the sandbox ran.
///
/// `bases/` and anything not named `sandbox-*` is never touched. One bad
/// entry never aborts the pass: failures are collected into `report.errors`.
pub fn sweep(data_root: &Path, active_ids: &[&str], cutoff: Duration) -> Result<GcReport> {
    sweep_impl(data_root, active_ids, cutoff, &|p| fs::remove_dir_all(p))
}

/// Sweep core with the removal primitive injected (tests inject failures to
/// prove per-entry collection; production uses `fs::remove_dir_all`).
fn sweep_impl(
    data_root: &Path,
    active_ids: &[&str],
    cutoff: Duration,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Result<GcReport> {
    let mut report = GcReport::default();
    let active: HashSet<&str> = active_ids.iter().copied().collect();
    let now = SystemTime::now();

    let rd = match fs::read_dir(data_root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(ImageError::from(e)),
    };
    for ent in rd {
        let ent = match ent {
            Ok(e) => e,
            Err(e) => {
                report
                    .errors
                    .push((data_root.to_path_buf(), format!("read_dir entry: {e}")));
                continue;
            }
        };
        let name = ent.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_prefix("sandbox-") else {
            continue; // bases/, stray files: out of sweep scope
        };
        let dir = ent.path();
        let marker = dir.join(INCOMPLETE_MARKER);
        if marker.is_file() {
            // ---- class 1: incomplete prepare debris
            if active.contains(id) {
                report.kept_active.push(dir);
                continue;
            }
            if !stale_mtime(&marker, now, cutoff) {
                report.kept_fresh.push(dir);
                continue;
            }
            match mount_leaks_under(&dir) {
                Ok(leaks) if !leaks.is_empty() => {
                    report.refused_mount_leak.push(dir);
                    continue; // audit ahead of deletion, always
                }
                Ok(_) => {}
                Err(e) => {
                    report.errors.push((dir, format!("mountinfo audit: {e}")));
                    continue;
                }
            }
            match remove(&dir) {
                Ok(()) => report.removed.push(dir),
                Err(e) => report
                    .errors
                    .push((dir, format!("incomplete debris removal: {e}"))),
            }
            continue;
        }
        let lease_path = dir.join(LEASE_FILE);
        if !lease_path.is_file() {
            continue; // neither incomplete nor committed: out of sweep scope
        }
        // ---- class 2: committed lease reconciliation (round-03 MAJOR #2)
        if active.contains(id) {
            report.kept_active.push(dir);
            continue;
        }
        let record = read_lease(&dir).ok(); // corrupt record => unknown owner
        let stale = match &record {
            Some(r) => {
                // cutoff ZERO = force; otherwise created_ms + cutoff <= now.
                cutoff == Duration::ZERO
                    || now
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|t| t.as_millis() as u64 >= r.created_ms.saturating_add(cutoff.as_millis() as u64))
                        .unwrap_or(false)
            }
            None => stale_mtime(&lease_path, now, cutoff),
        };
        if !stale {
            report.kept_fresh.push(dir);
            continue;
        }
        if record.as_ref().is_some_and(|r| pid_alive(r.owner_pid)) {
            report.kept_owner_live.push(dir);
            continue;
        }
        match mount_leaks_under(&dir) {
            Ok(leaks) if !leaks.is_empty() => {
                report.refused_mount_leak.push(dir);
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                report.errors.push((dir, format!("mountinfo audit: {e}")));
                continue;
            }
        }
        match remove(&dir) {
            Ok(()) => report.reclaimed_committed.push(dir),
            Err(e) => report
                .errors
                .push((dir, format!("committed lease reclamation: {e}"))),
        }
    }
    // deterministic outcome ordering (read_dir order is fs-dependent)
    report.removed.sort();
    report.reclaimed_committed.sort();
    report.kept_active.sort();
    report.kept_fresh.sort();
    report.kept_owner_live.sort();
    report.refused_mount_leak.sort();
    report.errors.sort();
    Ok(report)
}

/// Marker/lease file mtime-based staleness: `cutoff` ZERO matches ANY age
/// (operator force-clean); unmeasurable mtime is NOT stale (fail closed).
fn stale_mtime(path: &Path, now: SystemTime, cutoff: Duration) -> bool {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .map(|age| age >= cutoff)
        .unwrap_or(false)
}

/// Owner-pid liveness via `/proc` (Linux). See the sweep docs for the
/// documented pid-reuse caveat.
fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::unpack::UmociUnpacker;
    use crate::rootfs::prepare::{Driver, PrepareReport, RootfsManager};
    use crate::rootfs::testkit::{build_layout_with_layers, tar, tmpdir, Ent};

    #[test]
    fn mountinfo_parser_finds_only_descendants() {
        let content = "\
30 24 0:26 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw
31 24 0:27 / /work/data rw,relatime - overlay overlay rw
32 31 0:28 / /work/data/sandbox-a/rootfs rw,relatime - tmpfs tmpfs rw
33 31 0:29 / /work/data/sandbox-a/rootfs/dir\\040with\\040space rw - tmpfs tmpfs rw
34 31 0:30 / /work/data/sandbox-other/rootfs rw - tmpfs tmpfs rw
35 31 0:31 / /work/data2/sandbox-a-extra rw - tmpfs tmpfs rw
";
        let leaks = parse_mount_leaks(content, Path::new("/work/data/sandbox-a"));
        assert_eq!(
            leaks,
            vec![
                "/work/data/sandbox-a/rootfs".to_string(),
                "/work/data/sandbox-a/rootfs/dir with space".to_string(),
            ],
            "must match self + true descendants only (not -other, not prefix-sibling)"
        );
    }

    #[test]
    fn real_mountinfo_has_no_leaks_for_fresh_dir() {
        let d = tmpdir("nocleak");
        let leaks = mount_leaks_under(&d).unwrap();
        assert!(leaks.is_empty(), "fresh tmpdir must have no mounts: {leaks:?}");
    }

    fn make_incomplete(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(format!("sandbox-{id}"));
        fs::create_dir_all(&dir).unwrap();
        mark_incomplete(&dir).unwrap();
        dir
    }

    /// A pid that is guaranteed dead: spawn + reap, falling back to a value
    /// above any legal pid_max if the pid was recycled between wait and now.
    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn /bin/true");
        let pid = c.id();
        let _ = c.wait();
        if Path::new("/proc").join(pid.to_string()).exists() {
            u32::MAX // /proc/<u32::MAX> can never exist (pid_max <= 2^22)
        } else {
            pid
        }
    }

    fn write_committed(dir_name: &Path, owner_pid: u32, created_ms: u64) {
        fs::create_dir_all(dir_name).unwrap();
        let rec = LeaseRecord {
            lease_id: "lease-x".into(),
            sandbox_id: dir_name
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            base_digest: format!("sha256:{}", "0".repeat(64)),
            driver: "FULL_COPY".into(),
            rootfs: dir_name.join("rootfs"),
            owner_pid,
            created_ms,
        };
        fs::write(dir_name.join(LEASE_FILE), serde_json::to_vec_pretty(&rec).unwrap()).unwrap();
    }

    /// Round-03 MAJOR #2: committed lease trees MUST have a reclamation
    /// path. Dead owner + stale + inactive + no mounts => reclaimed; a live
    /// owner keeps the tree (the sandbox may legitimately hold it open).
    #[test]
    fn c04_sweep_reclaims_committed_lease_of_dead_owner_keeps_live() {
        let root = tmpdir("sweepcommit");
        let dead = root.join("sandbox-gone");
        write_committed(&dead, dead_pid(), 0); // epoch => stale under any cutoff
        fs::write(dead.join("junk"), b"j").unwrap();
        let live = root.join("sandbox-live");
        write_committed(&live, std::process::id(), 0);

        // cutoff ZERO = force path; staleness is exercised separately for
        // the incomplete class in c04_fresh_marker_kept_under_nonzero_cutoff.
        let rep = sweep(&root, &[], Duration::ZERO).unwrap();
        assert_eq!(rep.reclaimed_committed, vec![dead.clone()]);
        assert_eq!(rep.kept_owner_live, vec![live.clone()]);
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(!dead.exists(), "dead-owner committed tree must be reclaimed");
        assert!(live.exists(), "live-owner tree must survive even when stale");
        let _ = fs::remove_dir_all(&root);
    }

    /// Committed-class staleness: fresh leases (created_ms inside cutoff)
    /// are kept even with a dead owner; an unreadable lease.json is debris
    /// nobody can account for — reclaimed once stale (mtime-based).
    #[test]
    fn c04_committed_fresh_lease_kept_corrupt_record_reclaimed_when_stale() {
        let root = tmpdir("sweepcommit2");
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let fresh = root.join("sandbox-fresh");
        write_committed(&fresh, dead_pid(), now_ms);
        let corrupt = root.join("sandbox-corrupt");
        fs::create_dir_all(&corrupt).unwrap();
        fs::write(corrupt.join(LEASE_FILE), b"{not-a-record").unwrap();

        // nonzero cutoff: BOTH are fresh — the valid record via created_ms,
        // the corrupt record via lease.json mtime fallback.
        let rep = sweep(&root, &[], Duration::from_secs(3600)).unwrap();
        assert_eq!(rep.kept_fresh, vec![corrupt.clone(), fresh.clone()]);
        assert!(rep.reclaimed_committed.is_empty());
        assert!(fresh.exists() && corrupt.exists());

        // force cutoff (force-clean path): staleness lapses for every class
        // alike, so the corrupt record AND the fresh-but-dead-owner lease
        // are both reclaimed; only a LIVE owner still protects (covered in
        // the sibling test above).
        let rep2 = sweep(&root, &[], Duration::ZERO).unwrap();
        assert_eq!(rep2.reclaimed_committed, vec![corrupt.clone(), fresh.clone()]);
        assert!(!corrupt.exists() && !fresh.exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// Round-03 #5a: one per-entry IO failure must NOT abort the whole
    /// sweep; failures are collected into the report, other entries are
    /// still processed. The removal primitive is injected to make the
    /// failure deterministic on any host (permission tricks are root-flaky).
    #[test]
    fn c04_sweep_collects_per_entry_failures_and_continues() {
        let root = tmpdir("sweeperr");
        let bad = make_incomplete(&root, "bad");
        let good = make_incomplete(&root, "good");
        let bad_c = bad.clone();
        let rep = sweep_impl(&root, &[], Duration::ZERO, &move |p: &Path| {
            if p == bad_c {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected refusal (test)",
                ))
            } else {
                fs::remove_dir_all(p)
            }
        })
        .expect("sweep must not abort on one bad entry");
        assert!(
            rep.errors.iter().any(|(p, _)| p == &bad),
            "failure must be collected: {:?}",
            rep.errors
        );
        assert_eq!(rep.removed, vec![good.clone()], "other entries still processed");
        assert!(bad.exists());
        assert!(!good.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn c04_sweep_removes_stale_incomplete_keeps_active_committed_and_bases() {
        let root = tmpdir("sweep");
        let stale = make_incomplete(&root, "old");
        fs::write(stale.join("junk"), b"j").unwrap();
        let active = make_incomplete(&root, "live");
        // committed dir (lease.json, no marker) with a LIVE owner — kept by
        // the committed-reconciliation class even though it looks stale
        // (round-03 MAJOR #2: kept via kept_owner_live, not by blind skip).
        let committed = root.join("sandbox-done");
        write_committed(&committed, std::process::id(), 0);
        // shared bases must never be sweep scope
        let bases = root.join("bases");
        fs::create_dir_all(bases.join("abc")).unwrap();

        // cutoff ZERO = force-clean path; freshness protection is asserted
        // separately in c04_fresh_marker_kept_under_nonzero_cutoff.
        let rep = sweep(&root, &["live"], Duration::ZERO).unwrap();
        assert_eq!(rep.removed, vec![stale.clone()]);
        assert_eq!(rep.kept_active, vec![active.clone()]);
        assert_eq!(rep.kept_owner_live, vec![committed.clone()]);
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(!stale.exists(), "stale incomplete must be removed");
        assert!(active.exists(), "active must survive even when stale");
        assert!(committed.exists(), "live-owner committed lease must survive");
        assert!(bases.join("abc").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn c04_fresh_marker_kept_under_nonzero_cutoff() {
        let root = tmpdir("sweepfresh");
        let fresh = make_incomplete(&root, "new");
        let rep = sweep(&root, &[], Duration::from_secs(3600)).unwrap();
        assert!(rep.removed.is_empty());
        assert_eq!(rep.kept_fresh, vec![fresh.clone()]);
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// Real mount-leak evidence: on hosts where we may bind-mount (e.g.
    /// privileged hosts; THIS host denies mount(2) despite uid 0, so the
    /// test skips there), a leaked mount under a stale sandbox dir must
    /// REFUSE deletion and be reported — never silently removed. Skips
    /// honestly on hosts without mount privilege.
    #[test]
    fn c04_mount_leak_refuses_silent_deletion() {
        let root = tmpdir("mountleak");
        let stale = make_incomplete(&root, "leaky");
        let inner = stale.join("rootfs");
        fs::create_dir_all(&inner).unwrap();

        struct BindGuard(PathBuf, bool);
        impl BindGuard {
            fn mount(p: &Path) -> Option<BindGuard> {
                let ok = std::process::Command::new("mount")
                    .arg("--bind")
                    .arg(p)
                    .arg(p)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .ok()?
                    .success();
                if ok {
                    Some(BindGuard(p.to_path_buf(), true))
                } else {
                    None
                }
            }
        }
        impl Drop for BindGuard {
            fn drop(&mut self) {
                if self.1 {
                    let _ = std::process::Command::new("umount")
                        .arg(&self.0)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
        }

        let Some(_guard) = BindGuard::mount(&inner) else {
            eprintln!("SKIP c04-mount: bind mount not permitted on this host");
            let _ = fs::remove_dir_all(&root);
            return;
        };

        let leaks = mount_leaks_under(&stale).unwrap();
        assert!(!leaks.is_empty(), "live bind mount must be detected");

        let rep = sweep(&root, &[], Duration::ZERO).unwrap();
        assert_eq!(rep.refused_mount_leak, vec![stale.clone()]);
        assert!(rep.removed.is_empty(), "mount-leaked dir must never be deleted");
        assert!(stale.exists(), "mount-leaked dir must still exist");
        // guard Drop umounts; then cleanup is possible
        drop(_guard);
        let rep2 = sweep(&root, &[], Duration::ZERO).unwrap();
        assert_eq!(rep2.removed, vec![stale.clone()]);
        assert!(!stale.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn c04_release_removes_and_lease_record_round_trips() {
        let p = PathBuf::from("/opt/toolchains/lightsandbox/bin/umoci");
        if !p.is_file() {
            eprintln!("SKIP c04-release: pinned umoci absent on this host");
            return;
        }
        let layout = build_layout_with_layers(vec![tar(&[Ent::File {
            path: "x".into(),
            mode: 0o644,
            data: b"x".to_vec(),
        }])]);
        let image = crate::images::layout::load_verified(
            &layout.dir,
            Some("fixture:latest"),
            &crate::images::Platform::host(),
        )
        .unwrap();
        let root = tmpdir("release");
        let mgr = RootfsManager::new(root.clone(), UmociUnpacker::new(p));
        let lease = mgr
            .prepare(&layout.dir, "fixture:latest", &image, "rel", Driver::FullCopy)
            .unwrap();
        let dir = lease.sandbox_dir().to_path_buf();
        let rec = read_lease(&dir).unwrap();
        assert_eq!(rec.sandbox_id, "rel");
        assert_eq!(rec.driver, "FULL_COPY");
        assert!(rec.rootfs.ends_with("rootfs"));
        assert!(!dir.join(INCOMPLETE_MARKER).exists(), "committed lease must clear marker");
        lease.release().unwrap();
        assert!(!dir.exists(), "release must remove the sandbox dir");
        // a swept data root afterwards finds nothing to do
        let rep = sweep(&root, &[], Duration::ZERO).unwrap();
        assert!(rep.removed.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// Round-03 MAJOR #2 companion: commit publishes lease.json via
    /// tmp-file + rename so a crash can never leave a torn record, and no
    /// tmp residue remains on the happy path. Order is fixed: lease.json
    /// lands FIRST, `.incomplete` is cleared LAST — a crash between the two
    /// leaves a dir classified by the marker (incomplete debris), which is
    /// the sweep-safe fail-closed state.
    #[test]
    fn c04_commit_publishes_lease_atomically_no_tmp_residue() {
        let dir = tmpdir("commitatom");
        mark_incomplete(&dir).unwrap();
        let handle = RootfsHandle {
            lease_id: "lease-atom".into(),
            base_digest: format!("sha256:{}", "0".repeat(64)),
            driver: Driver::FullCopy,
            root: dir.join("rootfs"),
        };
        let report = PrepareReport {
            driver: "FULL_COPY",
            layer_count: 0,
            unpack_wall_ms: 0,
            copy_wall_ms: 0,
            logical_bytes: 0,
            allocated_delta_bytes: None,
            estimated_shared_bytes: None,
        };
        let lease = commit(&dir, handle, report).unwrap();
        assert_eq!(lease.record().lease_id, "lease-atom");
        assert!(dir.join(LEASE_FILE).is_file());
        assert!(!dir.join(INCOMPLETE_MARKER).exists());
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp residue after commit: {leftovers:?}");
        let rec = read_lease(&dir).unwrap();
        assert_eq!(rec.lease_id, "lease-atom");
        assert_eq!(rec.owner_pid, std::process::id());
        // dropping the lease without release leaves disk state for the sweep
        drop(lease);
        assert!(dir.join(LEASE_FILE).is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn c04_sweep_survives_unrelated_entries() {
        let root = tmpdir("sweepsafe");
        fs::write(root.join("stray.txt"), b"s").unwrap();
        fs::create_dir_all(root.join("not-a-sandbox")).unwrap();
        let rep = sweep(&root, &[], Duration::ZERO).unwrap();
        assert!(rep.removed.is_empty());
        assert!(root.join("stray.txt").exists());
        assert!(root.join("not-a-sandbox").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
