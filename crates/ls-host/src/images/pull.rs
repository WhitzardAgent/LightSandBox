//! Registry pull via mature transfer tools (T06-C05).
//!
//! Credential law (round-02 P2.1 / ADR-0001 amendment):
//! * credentials exist ONLY for the controlled host-side pull path;
//! * they are NEVER placed on a subprocess argv (visible in /proc cmdline),
//!   never written into image/exec/env, and never emitted to logs;
//! * skopeo consumes a Docker authfile written 0600 in a private tempdir and
//!   removed on scope exit — the path is on argv, the secret is not;
//! * errors from the tool are redacted against the credential value before
//!   they may surface anywhere.
//!
//! Live registry pulls additionally require a pinned tool; none is locked
//! for this host yet (see tests/images/README.md), so the live path reports
//! TransportUnavailable instead of improvising with an arbitrary binary.
//!
//! Spawn discipline (round-03 MAJOR #1): the tool's stdout/stderr go to
//! sidecar FILES, never pipes — output larger than the pipe capacity can
//! never deadlock a poll-only parent; bounded (redacted) tails are read
//! back only after the child exits or is killed + reaped.

use crate::images::error::{ImageError, Result};
use crate::images::layout::{list_descriptors, select_descriptor};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Registry credentials. The secret field is named `secret` on purpose and
/// never implements Display leakage: Debug is redacted.
pub struct RegistryCreds {
    pub username: String,
    secret: String,
}

impl RegistryCreds {
    pub fn new(username: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            secret: secret.into(),
        }
    }
}

impl std::fmt::Debug for RegistryCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryCreds")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// docker reference, e.g. `registry.example.com/ns/img:tag`.
    pub reference: String,
    /// Optional pinned `sha256:<hex>` manifest digest: the resolved digest
    /// MUST equal it, otherwise the pull fails (tag moved under us).
    pub pinned_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// local OCI layout dir containing the fetched image
    pub layout_dir: PathBuf,
    /// digest of the manifest actually resolved (tag -> digest binding)
    pub resolved_digest: String,
}

pub trait Transport {
    /// Fetch into `dest` (a fresh directory owned by the caller).
    /// `op_log` receives redacted, structurally-safe operation lines.
    fn fetch(
        &self,
        req: &PullRequest,
        dest: &Path,
        creds: Option<&RegistryCreds>,
        op_log: &mut Vec<String>,
    ) -> Result<PullOutcome>;
}

/// Strip any occurrence of the credential material from tool output before
/// it may be logged or wrapped in an error (defense in depth).
pub fn redact(text: &str, creds: Option<&RegistryCreds>) -> String {
    let mut out = text.to_string();
    if let Some(c) = creds {
        for piece in [&c.secret, &c.username] {
            if !piece.is_empty() {
                out = out.replace(piece, "<redacted>");
            }
        }
        // also redact the base64(auth) form, which is what an authfile-aware
        // error message might quote
        let b64 = base64_encode(format!("{}:{}", c.username, c.secret).as_bytes());
        out = out.replace(&b64, "<redacted>");
    }
    out
}

/// Registry host from a docker reference: first component when it looks
/// like a host (contains '.' or ':'), else the docker.io default.
pub fn registry_host(reference: &str) -> String {
    let first = reference.split('/').next().unwrap_or("");
    if first.contains('.') || first.contains(':') {
        first.to_string()
    } else {
        "registry-1.docker.io".to_string()
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TBL: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TBL[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TBL[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Write a Docker authfile 0600 holding ONLY the host-pull credential and
/// remove it on drop. The file path (not the secret) is argv-safe.
struct AuthFileGuard {
    dir: PathBuf,
    path: PathBuf,
}

impl AuthFileGuard {
    fn write(creds: &RegistryCreds, host: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "ls-pull-auth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // 0700 dir + 0600 create_new file at BIRTH (round-03 #3): no
        // umask-default 0644 window before a chmod could run, and never
        // follow/clobber a pre-existing path.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&dir)?;
        let path = dir.join("auth.json");
        let auth = base64_encode(format!("{}:{}", creds.username, creds.secret).as_bytes());
        let body = serde_json::json!({"auths": {host: {"auth": auth}}});
        let bytes = serde_json::to_vec(&body)?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            f.write_all(&bytes).map_err(ImageError::from)?;
        }
        #[cfg(not(unix))]
        fs::write(&path, bytes)?;
        Ok(Self { dir, path })
    }
}

impl Drop for AuthFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// --------------------------------------------------------------------- file

/// FileTransport: treats an existing OCI layout dir as the "registry" side.
/// Used by contract tests and by the controlled pre-stage host path; keeps
/// the same pull contract surface as the network transport.
pub struct FileTransport {
    pub source_layout: PathBuf,
}

impl Transport for FileTransport {
    fn fetch(
        &self,
        req: &PullRequest,
        dest: &Path,
        creds: Option<&RegistryCreds>,
        op_log: &mut Vec<String>,
    ) -> Result<PullOutcome> {
        if creds.is_some() {
            op_log.push("registry credentials: present (host-pull scope only)".to_string());
        }
        op_log.push(format!("transport: file copy from {}", self.source_layout.display()));
        copy_dir(&self.source_layout, dest)?;
        let descriptors = list_descriptors(dest)?;
        // resolve by the full annotated reference, else the sole manifest
        let resolved = select_descriptor(&descriptors, Some(req.reference.as_str()))
            .or_else(|_| select_descriptor(&descriptors, None))?;
        if let Some(pinned) = &req.pinned_digest {
            if *pinned != resolved.digest {
                return Err(ImageError::PinnedDigestMismatch {
                    pinned: pinned.clone(),
                    resolved: resolved.digest,
                });
            }
        }
        op_log.push(format!("resolved digest: {}", resolved.digest));
        Ok(PullOutcome {
            layout_dir: dest.to_path_buf(),
            resolved_digest: resolved.digest,
        })
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let target = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- skopeo

/// SkopeoTransport: network pulls through a pinned skopeo binary. Never
/// passes credentials on argv; consumes a 0600 authfile instead.
pub struct SkopeoTransport {
    pub binary: PathBuf,
    pub timeout_ms: u64,
}

impl SkopeoTransport {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout_ms: 120_000,
        }
    }

    /// The exact argv we would run. Secret material never appears here;
    /// the authfile PATH does. Tested as a pure function.
    pub fn argv_for(&self, req: &PullRequest, authfile: Option<&Path>, dest: &Path) -> Vec<String> {
        let mut argv = vec![
            self.binary.to_string_lossy().to_string(),
            "copy".to_string(),
        ];
        if let Some(p) = authfile {
            argv.push("--authfile".to_string());
            argv.push(p.to_string_lossy().to_string());
        }
        argv.push(format!("docker://{}", req.reference));
        argv.push(format!("oci:{}", dest.display()));
        argv
    }
}

impl Transport for SkopeoTransport {
    fn fetch(
        &self,
        req: &PullRequest,
        dest: &Path,
        creds: Option<&RegistryCreds>,
        op_log: &mut Vec<String>,
    ) -> Result<PullOutcome> {
        if !self.binary.is_file() {
            return Err(ImageError::TransportUnavailable {
                tool: format!("skopeo (no pinned binary at {})", self.binary.display()),
            });
        }
        let host = registry_host(&req.reference);
        let auth = match creds {
            Some(c) => Some(AuthFileGuard::write(c, &host)?),
            None => None,
        };
        let argv = self.argv_for(req, auth.as_ref().map(|a| a.path.as_path()), dest);
        op_log.push(format!(
            "exec argv (credential-free): {}",
            argv.join(" ")
        ));
        // stdout/stderr go to sidecar FILES, never pipes (round-03 MAJOR #1,
        // same pattern as unpack.rs): a tool emitting more than the pipe
        // capacity on a never-drained pipe blocks in write(2) forever and a
        // poll-only parent would kill the healthy child as a spurious
        // "timeout". Files have no such bound; bounded tails are read back
        // only after the child exits or is reaped.
        let sides = PullSidecars::create(dest).map_err(|e| ImageError::Transport {
            detail: format!("skopeo sidecar setup failed: {e}"),
        })?;
        let streams = sides.stdout_file().and_then(|o| sides.stderr_file().map(|e| (o, e)));
        let (out_f, err_f) = match streams {
            Ok(pair) => pair,
            Err(e) => {
                sides.cleanup();
                return Err(ImageError::Transport {
                    detail: format!("skopeo sidecar open failed: {e}"),
                });
            }
        };
        let spawn = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_f))
            .stderr(Stdio::from(err_f))
            // scrub the tool's environment: a transfer helper must not
            // inherit ambient secrets from the manager process either
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .spawn();
        let mut child = match spawn {
            Ok(c) => c,
            Err(e) => {
                sides.cleanup();
                return Err(ImageError::Transport {
                    detail: format!("skopeo spawn failed: {e}"),
                });
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(self.timeout_ms);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Ok(None) => {
                    // SIGKILL cannot be caught, so wait() below is bounded:
                    // the child is always reaped, never left as a zombie.
                    let _ = child.kill();
                    let _ = child.wait();
                    sides.cleanup();
                    return Err(ImageError::Transport {
                        detail: format!("skopeo timed out after {}ms", self.timeout_ms),
                    });
                }
                Err(e) => {
                    sides.cleanup();
                    return Err(ImageError::Transport {
                        detail: format!("skopeo wait failed: {e}"),
                    })
                }
            }
        };
        if !status.success() {
            let stderr = redact(&read_tail(&sides.stderr_path, STDERR_TAIL), creds);
            sides.cleanup();
            return Err(ImageError::Transport {
                detail: format!("skopeo exit {:?}: {}", status.code(), stderr.trim()),
            });
        }
        // success path: keep the tool's (bounded, redacted) stderr on the
        // op record instead of discarding it unread — tool warnings would
        // otherwise leave no trace.
        let tail = redact(&read_tail(&sides.stderr_path, STDERR_TAIL), creds);
        sides.cleanup();
        if !tail.trim().is_empty() {
            op_log.push(format!("skopeo stderr (bounded tail): {}", tail.trim()));
        }
        let descriptors = list_descriptors(dest)?;
        let resolved = select_descriptor(&descriptors, None)?;
        if let Some(pinned) = &req.pinned_digest {
            if *pinned != resolved.digest {
                return Err(ImageError::PinnedDigestMismatch {
                    pinned: pinned.clone(),
                    resolved: resolved.digest,
                });
            }
        }
        op_log.push(format!("resolved digest: {}", resolved.digest));
        Ok(PullOutcome {
            layout_dir: dest.to_path_buf(),
            resolved_digest: resolved.digest,
        })
    }
}

// ------------------------------------------------------------- sidecars

/// Bounded stderr tail read back from the sidecar file after the child
/// exits or is reaped.
const STDERR_TAIL: u64 = 8 * 1024;

/// Sidecar stdout/stderr files for one tool invocation (pipe-drain deadlock
/// avoidance — round-03 MAJOR #1). Unique per call (pid + nanos), placed
/// next to the destination, and always cleaned up: no residue on success,
/// timeout or error paths.
struct PullSidecars {
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl PullSidecars {
    fn create(dest: &Path) -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let parent = dest
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&parent)?;
        let mk = |stream: &str| {
            parent.join(format!(
                ".ls-pull-{stream}-{}-{nanos}",
                std::process::id()
            ))
        };
        let s = Self {
            stdout_path: mk("stdout"),
            stderr_path: mk("stderr"),
        };
        // create both now so spawn Stdio::from() cannot fail halfway
        fs::File::create(&s.stdout_path)?;
        fs::File::create(&s.stderr_path)?;
        Ok(s)
    }

    fn stdout_file(&self) -> std::io::Result<fs::File> {
        fs::OpenOptions::new().append(true).open(&self.stdout_path)
    }

    fn stderr_file(&self) -> std::io::Result<fs::File> {
        fs::OpenOptions::new().append(true).open(&self.stderr_path)
    }

    fn cleanup(&self) {
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

/// Read at most the last `limit` bytes of a sidecar file (best-effort).
fn read_tail(path: &Path, limit: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let skip = len.saturating_sub(limit);
    let _ = f.seek(SeekFrom::Start(skip));
    let mut buf = Vec::new();
    let _ = f.by_ref().take(limit).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::testkit::{build_layout, tmpdir};
    use serde_json::json;

    #[test]
    fn base64_kat_rfc4648() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn registry_host_parsing() {
        assert_eq!(registry_host("registry.example.com/ns/img:tag"), "registry.example.com");
        assert_eq!(registry_host("localhost:5000/img:tag"), "localhost:5000");
        assert_eq!(registry_host("library/alpine:latest"), "registry-1.docker.io");
    }

    // ------------------------------------------------------------- T06-C05

    #[test]
    fn c05_file_transport_resolves_tag_to_digest_and_enforces_pin() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        let dest = tmpdir("pull-dest");
        let mut log = Vec::new();
        let t = FileTransport {
            source_layout: img.dir.clone(),
        };
        let req = PullRequest {
            reference: "fixture:latest".into(),
            pinned_digest: Some(format!("sha256:{}", img.manifest_digest)),
        };
        let out = t.fetch(&req, &dest, None, &mut log).unwrap();
        assert_eq!(out.resolved_digest, format!("sha256:{}", img.manifest_digest));
        assert!(dest.join("index.json").is_file());

        // a moved/wrong pin must fail, not silently accept
        let bad = PullRequest {
            reference: "fixture:latest".into(),
            pinned_digest: Some(format!("sha256:{}", "0".repeat(64))),
        };
        let dest2 = tmpdir("pull-dest2");
        let err = t.fetch(&bad, &dest2, None, &mut log).unwrap_err();
        assert_eq!(err.code(), "IMAGE_PINNED_DIGEST_MISMATCH");
        let _ = fs::remove_dir_all(&dest);
        let _ = fs::remove_dir_all(&dest2);
    }

    #[test]
    fn c05_credentials_reach_neither_argv_nor_logs_nor_errors() {
        let secret = "S3cret-pull-token-LSBX";
        let creds = RegistryCreds::new("puller", secret);
        let dest = PathBuf::from("/tmp/ls-dest-x");
        let t = SkopeoTransport::new("/opt/toolchains/lightsandbox/bin/skopeo");
        let req = PullRequest {
            reference: "registry.example.com/ns/img:tag".into(),
            pinned_digest: None,
        };
        // argv construction: authfile path on argv, secret NEVER
        let auth = AuthFileGuard::write(&creds, "registry.example.com").unwrap();
        let argv = t.argv_for(&req, Some(&auth.path), &dest);
        let joined = argv.join(" ");
        assert!(!joined.contains(secret), "secret leaked into argv: {joined}");
        assert!(argv.iter().any(|a| a == "--authfile"));
        // authfile content is the ONLY secret-bearing artifact, mode 0600
        let body: serde_json::Value =
            serde_json::from_slice(&fs::read(&auth.path).unwrap()).unwrap();
        let b64 = body["auths"]["registry.example.com"]["auth"].as_str().unwrap();
        assert_eq!(
            b64,
            base64_encode(format!("puller:{secret}").as_bytes())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&auth.path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // redaction scrubs secret, username and the b64 form from tool noise
        let noisy = format!("unauthorized for puller with {b64} using {secret} blah");
        let redacted = redact(&noisy, Some(&creds));
        assert!(!redacted.contains(secret));
        assert!(!redacted.contains("puller"));
        assert!(!redacted.contains(b64));
        // Debug of the creds themselves is redacted
        assert!(!format!("{creds:?}").contains(secret));
        // guard-drop removes the only secret-bearing artifact from disk
        let auth_dir = auth.dir.clone();
        drop(auth);
        assert!(!auth_dir.exists(), "authfile dir must be removed on drop");
    }

    /// Round-03 MINOR #3: the credential artifact must be private from BIRTH —
    /// dir 0700, file created 0600 via create_new (no 0644 umask window and
    /// never following/clobbering a pre-existing path).
    #[test]
    fn c05_authfile_dir_and_file_modes_are_private_from_birth() {
        let creds = RegistryCreds::new("puller", "tok");
        let auth = AuthFileGuard::write(&creds, "registry.example.com").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&auth.dir).unwrap().permissions().mode() & 0o777,
                0o700,
                "auth dir must be 0700"
            );
            assert_eq!(
                fs::metadata(&auth.path).unwrap().permissions().mode() & 0o777,
                0o600,
                "authfile must be 0600 at creation (no post-hoc chmod window)"
            );
        }
    }

    /// Round-03 MAJOR #1 regression: a skopeo child emitting far more than
    /// the 64 KiB pipe capacity on BOTH stdout and stderr must complete —
    /// piped-but-never-drained streams block the child in write(2) forever
    /// and a poll-only parent would spuriously kill it as a "timeout".
    /// Evidence shape: this test FAILS (timeout) with a never-drained pipe
    /// design and PASSES once output goes to sidecar files.
    #[test]
    fn c05_large_child_output_never_deadlocks_pull() {
        let dir = tmpdir("pull-deadlock");
        let script = dir.join("fake-skopeo.sh");
        // 4 MiB per stream: >> any plausible default pipe capacity, so a
        // non-draining parent deterministically stalls the child.
        fs::write(
            &script,
            br#"#!/bin/sh
head -c 4194304 /dev/zero
head -c 4194304 /dev/zero 1>&2
last=""
for a in "$@"; do last="$a"; done
dest="${last#oci:}"
mkdir -p "$dest"
printf '%s' '{"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":7}]}' > "$dest/index.json"
exit 0
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut t = SkopeoTransport::new(&script);
        // a drained child finishes in milliseconds; 3 s is generous headroom
        // yet turns any drain regression into a fast, deterministic failure.
        t.timeout_ms = 3_000;
        let req = PullRequest {
            reference: "registry.example.com/ns/img:tag".into(),
            pinned_digest: None,
        };
        let dest = tmpdir("pull-deadlock-dest");
        let mut log = Vec::new();
        let out = t.fetch(&req, &dest, None, &mut log).unwrap_or_else(|e| {
            panic!("pipe-full must not masquerade as timeout (drain design): {e}")
        });
        assert_eq!(out.resolved_digest, format!("sha256:{}", "1".repeat(64)));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn c05_missing_pinned_tool_reports_unavailable_not_silent_fallback() {
        let t = SkopeoTransport::new("/definitely/not/pinned/skopeo-t06");
        let req = PullRequest {
            reference: "registry.example.com/ns/img:tag".into(),
            pinned_digest: None,
        };
        let dest = tmpdir("pull-no-tool");
        let mut log = Vec::new();
        let err = t.fetch(&req, &dest, None, &mut log).unwrap_err();
        assert_eq!(err.code(), "IMAGE_TRANSPORT_UNAVAILABLE");
        // nothing was fetched and no executable fallback was attempted
        assert!(log.is_empty());
        let _ = fs::remove_dir_all(&dest);
    }
}
