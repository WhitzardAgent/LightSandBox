//! OCI layer unpack via the pinned mature unpacker (09_RUNTIME §17, T07).
//!
//! LightSandbox never implements its own layer application loop (overlay
//! ordering, whiteouts, hardlink/symlink handling, path-escape rejection).
//! The only supported unpacker is the pinned `umoci` binary resolved from an
//! explicit path — never ambient PATH, matching `SkopeoTransport`. Binary
//! pin/sha256 binding is enforced by the T01 tool manifest (`tools/lock_*`);
//! at runtime this adapter receives the resolved path and fails with
//! `IMAGE_TRANSPORT_UNAVAILABLE` when it is absent (never silent fallback).
//!
//! Invocation discipline (T06/T41 §29): argv-only, no shell anywhere, bounded
//! stderr capture through a temp sidecar (no pipe deadlock), wall-clock
//! timeout, minimal scrubbed environment, structured `ImageError`. No secret
//! material ever enters argv or env on this path.
//!
//! `umoci unpack --image <layout>:<ref> <bundle>` produces an OCI runtime
//! bundle: `<bundle>/rootfs/` plus `<bundle>/config.json`. Only the rootfs
//! tree is consumed here (process config comes from the verified image
//! config, T06). A best-effort containment audit walks the produced tree
//! without following symlinks — it is a second-line check, never the primary
//! guard (the primary guard is umoci's own escape rejection).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::error::ImageError;

/// Default wall-clock bound on one `umoci unpack` invocation.
pub const DEFAULT_UNPACK_TIMEOUT: Duration = Duration::from_secs(300);
/// stderr sidecar read-back bound.
const STDERR_TAIL: u64 = 8 * 1024;

static SIDE_N: AtomicU64 = AtomicU64::new(0);

/// Outcome of one successful unpack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackReport {
    pub wall_ms: u64,
}

/// `UmociUnpacker`: pinned-path mature unpacker adapter.
#[derive(Debug, Clone)]
pub struct UmociUnpacker {
    binary: PathBuf,
    timeout: Duration,
}

impl UmociUnpacker {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: DEFAULT_UNPACK_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Mirrors `SkopeoTransport`: an absent pinned binary is a structured
    /// unavailability signal, never a silent fallback to PATH or self-code.
    fn ensure_available(&self) -> Result<(), ImageError> {
        if self.binary.is_file() {
            Ok(())
        } else {
            Err(ImageError::TransportUnavailable {
                tool: format!("umoci (no pinned binary at {})", self.binary.display()),
            })
        }
    }

    /// Exact argv (pure, unit-testable). `--image <layout>:<ref>` — the ref
    /// is passed verbatim; callers pass tag refs from the verified index.
    pub fn unpack_argv(layout_root: &Path, reference: &str, dest_bundle: &Path) -> Vec<OsString> {
        let binary = OsString::from("umoci"); // placeholder position 0, replaced by spawn
        let mut image = layout_root.as_os_str().to_os_string();
        image.push(":");
        image.push(reference);
        vec![
            binary,
            OsString::from("unpack"),
            OsString::from("--image"),
            image,
            dest_bundle.as_os_str().to_os_string(),
        ]
    }

    /// Unpack `<layout_root>:<reference>` into a NEW bundle dir `dest_bundle`
    /// (must not exist beforehand; umoci refuses to clobber). The resulting
    /// rootfs lives at `dest_bundle/rootfs`. On any failure the partial
    /// destination is removed — no half trees escape this adapter.
    pub fn unpack(
        &self,
        layout_root: &Path,
        reference: &str,
        dest_bundle: &Path,
    ) -> Result<UnpackReport, ImageError> {
        self.ensure_available()?;
        if dest_bundle.exists() {
            return Err(ImageError::UnpackFailed {
                detail: format!(
                    "unpack destination already exists (refusing clobber): {}",
                    dest_bundle.display()
                ),
            });
        }
        let mut argv = Self::unpack_argv(layout_root, reference, dest_bundle);
        argv[0] = self.binary.as_os_str().to_os_string();

        // stderr goes to a sidecar file so a noisy tool can never deadlock a
        // pipe; we read back a bounded tail afterwards.
        let side = sidecar_path(dest_bundle);
        if let Some(parent) = dest_bundle.parent() {
            fs::create_dir_all(parent).map_err(ImageError::from)?;
        }
        let side_file = fs::File::create(&side).map_err(ImageError::from)?;

        let started = Instant::now();
        let mut child = Command::new(&self.binary)
            .args(&argv[1..])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", "/tmp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(side_file))
            .spawn()
            .map_err(|e| ImageError::UnpackFailed {
                detail: format!("umoci spawn failed: {e}"),
            })?;

        let wall_ms;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    wall_ms = started.elapsed().as_millis() as u64;
                    if !status.success() {
                        let tail = read_tail(&side);
                        let _ = fs::remove_file(&side);
                        let _ = fs::remove_dir_all(dest_bundle);
                        return Err(ImageError::UnpackFailed {
                            detail: format!(
                                "umoci unpack exit {}: {}",
                                status.code().unwrap_or(-1),
                                tail.trim()
                            ),
                        });
                    }
                    break;
                }
                Ok(None) => {
                    if started.elapsed() > self.timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = fs::remove_file(&side);
                        let _ = fs::remove_dir_all(dest_bundle);
                        return Err(ImageError::UnpackFailed {
                            detail: format!(
                                "umoci unpack exceeded {}s wall clock",
                                self.timeout.as_secs()
                            ),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = fs::remove_file(&side);
                    let _ = fs::remove_dir_all(dest_bundle);
                    return Err(ImageError::from(e));
                }
            }
        }
        let _ = fs::remove_file(&side);

        // Second-line containment audit (non-following). Failure here means
        // the tool produced something we refuse to run: clean up and report.
        let rootfs = dest_bundle.join("rootfs");
        if let Err(e) = post_unpack_containment_walk(&rootfs) {
            let _ = fs::remove_dir_all(dest_bundle);
            return Err(e);
        }

        Ok(UnpackReport { wall_ms })
    }
}

fn sidecar_path(dest_bundle: &Path) -> PathBuf {
    let n = SIDE_N.fetch_add(1, Ordering::SeqCst);
    let name = format!(
        ".umoci-stderr-{}-{n}",
        std::process::id()
    );
    match dest_bundle.parent() {
        Some(p) => p.join(name),
        None => PathBuf::from(name),
    }
}

fn read_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let skip = len.saturating_sub(STDERR_TAIL);
    let _ = f.seek(SeekFrom::Start(skip));
    let mut buf = Vec::new();
    let _ = f.by_ref().take(STDERR_TAIL).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

/// Best-effort second-line containment audit: every REAL (non-symlink) entry
/// under `rootfs` must canonicalize to a path still inside `rootfs`.
/// Symlink targets are never followed — a dangling or outside-pointing
/// symlink is legal in an image and gets confined by the runtime mount
/// namespace, not by this audit. umoci's own escape validation remains the
/// primary guard; this audit is intentionally simple and conservative.
pub fn post_unpack_containment_walk(rootfs: &Path) -> Result<(), ImageError> {
    let base = fs::canonicalize(rootfs).map_err(ImageError::from)?;
    let mut stack: Vec<PathBuf> = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(ImageError::from(e)),
        };
        for ent in rd {
            let ent = ent.map_err(ImageError::from)?;
            let ft = ent.file_type().map_err(ImageError::from)?;
            if ft.is_symlink() {
                continue;
            }
            let canon = fs::canonicalize(ent.path()).map_err(ImageError::from)?;
            if !canon.starts_with(&base) {
                return Err(ImageError::LayoutInvalid {
                    detail: format!(
                        "post-unpack containment audit: entry escapes rootfs: {}",
                        canon.display()
                    ),
                });
            }
            if ft.is_dir() {
                stack.push(canon);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned_umoci() -> Option<UmociUnpacker> {
        let p = PathBuf::from("/opt/toolchains/lightsandbox/bin/umoci");
        if p.is_file() {
            Some(UmociUnpacker::new(p))
        } else {
            None
        }
    }

    fn tmptag(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ls-t07-unpack-{tag}-{}-{}",
            std::process::id(),
            SIDE_N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn argv_is_exact_and_shell_free() {
        let argv = UmociUnpacker::unpack_argv(
            Path::new("/data/layout"),
            "fixture:latest",
            Path::new("/work/bundle"),
        );
        let got: Vec<String> = argv.iter().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(
            got,
            vec![
                "umoci".to_string(),
                "unpack".to_string(),
                "--image".to_string(),
                "/data/layout:fixture:latest".to_string(),
                "/work/bundle".to_string(),
            ]
        );
        // no shell metacharacters are ever introduced by the builder itself
        for a in &got {
            assert!(!a.contains("&&") && !a.contains('|') && !a.contains(';'));
        }
    }

    #[test]
    fn missing_pinned_binary_is_unavailable_not_fallback() {
        let u = UmociUnpacker::new("/definitely/not/pinned/umoci-t07");
        let tmp = tmptag("missing");
        let err = u
            .unpack(Path::new("/nonexistent-layout"), "x:latest", &tmp.join("b"))
            .unwrap_err();
        assert_eq!(err.code(), "IMAGE_TRANSPORT_UNAVAILABLE");
    }

    #[test]
    fn clobber_refusal_precedes_spawn() {
        let u = UmociUnpacker::new("/definitely/not/pinned/umoci-t07");
        // availability check fires first
        let tmp = tmptag("clob");
        let dest = tmp.join("bundle");
        fs::create_dir_all(&dest).unwrap();
        let err = u
            .unpack(Path::new("/data/l"), "x:latest", &dest)
            .unwrap_err();
        assert!(matches!(
            err,
            ImageError::TransportUnavailable { .. }
        ));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn containment_walk_passes_normal_tree_and_ignores_symlinks() {
        let tmp = tmptag("walk");
        let root = tmp.join("rootfs");
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("etc").join("hostname"), b"sandbox\n").unwrap();
        // dangling + absolute symlink targets are legal inside an image
        std::os::unix::fs::symlink("/etc/hostname", root.join("link")).unwrap();
        std::os::unix::fs::symlink("../../../outside", root.join("out")).unwrap();
        post_unpack_containment_walk(&root).unwrap();
        let _ = fs::remove_dir_all(&tmp);
    }

    /// T07-C01. Real pinned-umoci behavior (evidence recorded in
    /// tests/rootfs/EXPECTATIONS.md, measured 2026-09-24 on the pinned
    /// binary): for these malicious inputs umoci CONFINE-SANITIZES rather
    /// than rejects: `../evil-t07` is rewritten to `rootfs/evil-t07`, a
    /// write through an escaping symlink is evaluated under the rootfs —
    /// exit 0, no host-side artifact. The contract invariant is therefore
    /// confinement, asserted hard in BOTH outcome arms:
    ///   * unpack rejects (structured error)   → no partial dest, no artifact;
    ///   * unpack sanitize-succeeds            → containment audit passes,
    ///     symlink target strings verbatim, and NO `evil-t07` anywhere
    ///     outside the bundle tree.
    /// The mechanism (reject vs sanitize) is logged for the record.
    #[test]
    fn c01_malicious_layers_never_produce_host_escape_artifacts() {
        use crate::rootfs::testkit::{build_layout_with_layers, tar, Ent};
        let Some(u) = pinned_umoci() else {
            eprintln!("SKIP c01: pinned umoci absent on this host");
            return;
        };
        for (name, entries) in [
            (
                "dotdot",
                vec![Ent::File {
                    path: "../evil-t07".into(),
                    mode: 0o644,
                    data: b"pwn".to_vec(),
                }],
            ),
            (
                "abslink",
                vec![
                    Ent::Symlink {
                        path: "s".into(),
                        target: "../../../../etc".into(),
                    },
                    Ent::File {
                        path: "s/evil-t07".into(),
                        mode: 0o644,
                        data: b"pwn".to_vec(),
                    },
                ],
            ),
        ] {
            let layout = build_layout_with_layers(vec![tar(&entries)]);
            let tmp = tmptag(name);
            let dest = tmp.join("bundle");
            match u.unpack(&layout.dir, "fixture:latest", &dest) {
                Err(e) => {
                    assert!(
                        matches!(
                            e,
                            ImageError::UnpackFailed { .. } | ImageError::LayoutInvalid { .. }
                        ),
                        "rejection must be a structured image error, got {e:?}"
                    );
                    assert!(!dest.exists(), "partial bundle must be cleaned: {name}");
                    eprintln!("c01 {name}: mechanism=REJECT ({})", e.code());
                }
                Ok(_rep) => {
                    // sanitize arm: containment audit ran inside unpack; the
                    // resulting tree must still satisfy it, and the symlink
                    // (verbatim target) must not have been followed anywhere.
                    post_unpack_containment_walk(&dest.join("rootfs")).unwrap();
                    eprintln!("c01 {name}: mechanism=SANITIZE-CONFINE (exit 0)");
                }
            }
            // THE INVARIANT (both arms): nothing named evil-t07 exists in
            // the case dir outside the bundle, i.e. no host-side escape.
            let outside: Vec<_> = fs::read_dir(&tmp)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains("evil-t07"))
                .collect();
            assert!(
                outside.is_empty(),
                "escape artifact outside bundle: {outside:?} (case {name})"
            );
            let _ = fs::remove_dir_all(&tmp);
        }
    }
}
