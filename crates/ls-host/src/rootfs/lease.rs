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
//! * The GC sweep touches ONLY `sandbox-*` dirs, only those still carrying
//!   `.incomplete`, only older than a caller cutoff, never any dir named in
//!   `active_ids`, and NEVER anything with a live mount reference under it
//!   (`/proc/self/mountinfo` audit) — a mount leak is reported, not silently
//!   deleted. `bases/` (shared per-digest verified bases) is outside sweep
//!   scope by construction.

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

/// Commit a lease: write lease.json, clear `.incomplete`, return the live
/// handle. Called once by `RootfsManager::prepare` after the copy driver
/// succeeds.
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
    fs::write(sandbox_dir.join(LEASE_FILE), json).map_err(ImageError::from)?;
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
    pub removed: Vec<PathBuf>,
    pub kept_active: Vec<PathBuf>,
    pub kept_fresh: Vec<PathBuf>,
    pub refused_mount_leak: Vec<PathBuf>,
}

/// Sweep `data_root`: remove only `sandbox-*` dirs that
///   (a) still carry `.incomplete` (prepared never committed), AND
///   (b) are NOT listed in `active_ids`, AND
///   (c) the marker is at least `cutoff` old — NOTE: `Duration::ZERO`
///       matches ANY age (force-clean path used by tests/operators); crash
///       debris protection for in-flight prepares REQUIRES a nonzero
///       cutoff in production callers, AND
///   (d) have no live mount reference underneath.
/// `bases/` and anything not named `sandbox-*` is never touched.
pub fn sweep(data_root: &Path, active_ids: &[&str], cutoff: Duration) -> Result<GcReport> {
    let mut report = GcReport::default();
    let active: HashSet<&str> = active_ids.iter().copied().collect();
    let now = SystemTime::now();

    let rd = match fs::read_dir(data_root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(ImageError::from(e)),
    };
    for ent in rd {
        let ent = ent.map_err(ImageError::from)?;
        let name = ent.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_prefix("sandbox-") else {
            continue; // bases/, stray files: out of sweep scope
        };
        let dir = ent.path();
        let marker = dir.join(INCOMPLETE_MARKER);
        if !marker.is_file() {
            continue; // committed lease (or empty): not crash debris
        }
        if active.contains(id) {
            report.kept_active.push(dir);
            continue;
        }
        let stale = marker
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age >= cutoff)
            .unwrap_or(false);
        if !stale {
            report.kept_fresh.push(dir);
            continue;
        }
        let leaks = mount_leaks_under(&dir)?;
        if !leaks.is_empty() {
            report.refused_mount_leak.push(dir);
            continue; // audit ahead of deletion, always
        }
        fs::remove_dir_all(&dir).map_err(ImageError::from)?;
        report.removed.push(dir);
    }
    // deterministic outcome ordering (read_dir order is fs-dependent)
    report.removed.sort();
    report.kept_active.sort();
    report.kept_fresh.sort();
    report.refused_mount_leak.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::unpack::UmociUnpacker;
    use crate::rootfs::prepare::{Driver, RootfsManager};
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

    #[test]
    fn c04_sweep_removes_stale_incomplete_keeps_active_committed_and_bases() {
        let root = tmpdir("sweep");
        let stale = make_incomplete(&root, "old");
        fs::write(stale.join("junk"), b"j").unwrap();
        let active = make_incomplete(&root, "live");
        // committed dir (lease.json, no marker) — must be left alone even
        // though it looks like a sandbox dir
        let committed = root.join("sandbox-done");
        fs::create_dir_all(&committed).unwrap();
        fs::write(committed.join(LEASE_FILE), b"{}").unwrap();
        // shared bases must never be sweep scope
        let bases = root.join("bases");
        fs::create_dir_all(bases.join("abc")).unwrap();

        // cutoff ZERO = force-clean path; freshness protection is asserted
        // separately in c04_fresh_marker_kept_under_nonzero_cutoff.
        let rep = sweep(&root, &["live"], Duration::ZERO).unwrap();
        assert_eq!(rep.removed, vec![stale.clone()]);
        assert_eq!(rep.kept_active, vec![active.clone()]);
        assert!(!stale.exists(), "stale incomplete must be removed");
        assert!(active.exists(), "active must survive even when stale");
        assert!(committed.exists());
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

    /// Real mount-leak evidence: on hosts where we may bind-mount (uid 0
    /// here), a leaked mount under a stale sandbox dir must REFUSE deletion
    /// and be reported — never silently removed. Skips honestly on hosts
    /// without mount privilege.
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
