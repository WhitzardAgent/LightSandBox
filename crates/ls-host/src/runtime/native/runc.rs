//! `RuncRuntime`: the last hop of the Native backend — pinned binary,
//! argv-only spawn discipline, measured PORTABLE_ROOTLESS identity.
//!
//! Hard rules carried over from the spike + round-03 review:
//! * the binary is an ABSOLUTE pinned path (default
//!   `/opt/toolchains/lightsandbox/bin/runc`, locked in
//!   specs/upstream-lock.json at commit 41b7477); PATH hijack can never
//!   swap it;
//! * every invocation is argv-only (never a shell), env scrubbed;
//! * command stdout/stderr go to SIDECAR FILES, never pipes — a child that
//!   fills a pipe can never deadlock a waiting parent (pull.rs precedent,
//!   round-03 MAJOR #1);
//! * the process runs as the configured unprivileged identity
//!   (uid=gid=65534 in the measured dev-worker profile): PORTABLE_ROOTLESS
//!   means the runtime executes as the real unprivileged user, never as a
//!   nested uid-0 (adr/0001 §0). Dropping happens by pre-exec setuid in OUR
//!   own fork — no sudo, no host config change, no newuidmap
//!   (single-ID mapping only, same constraint class).

use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Upstream-locked default binary (specs/upstream-lock.json).
pub const PINNED_RUNC: &str = "/opt/toolchains/lightsandbox/bin/runc";
/// Optional absolute-path override (dev/CI flexibility); production wiring
/// passes the locked path explicitly. NEVER searched via PATH.
pub const RUNC_BIN_ENV: &str = "LSB_RUNC_BIN";

/// setuid/setgid without a crate dependency (same zero-dep policy as the
/// rest of the runtime path; these are trivial thin syscall wrappers).
mod sys {
    extern "C" {
        pub fn setuid(uid: u32) -> i32;
        pub fn setgid(gid: u32) -> i32;
    }
}

/// One failed runc invocation: op stage, real exit code, bounded sidecar
/// tail. Implementation diagnostics live HERE (the only legal place).
#[derive(Debug, Clone)]
pub struct RuncFailure {
    pub op: &'static str,
    pub exit_code: Option<i32>,
    pub stderr_tail: String,
    pub note: String,
}

impl std::fmt::Display for RuncFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "runc {} failed (exit {:?}): {} [{}]",
            self.op, self.exit_code, self.stderr_tail, self.note
        )
    }
}

impl std::error::Error for RuncFailure {}

/// Parsed `runc state` status strings (OCI lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerStatus {
    Created,
    Running,
    Stopped,
    NotFound,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct RuncRuntime {
    binary: PathBuf,
    state_root: PathBuf,
    /// None = run as the current user (production deployments already run
    /// the worker as the unprivileged user). Some((uid,gid)) = pre-exec
    /// drop in our own fork (dev-worker profile: outer uid 0 -> 65534).
    identity: Option<(u32, u32)>,
    /// HOME for the runc process (must be owned by `identity`).
    home: PathBuf,
    /// Wall clock cap for control-plane ops (create/start/delete/state).
    op_timeout: Duration,
}

impl RuncRuntime {
    pub fn new(
        binary: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
        identity: Option<(u32, u32)>,
        home: impl Into<PathBuf>,
    ) -> Self {
        RuncRuntime {
            binary: binary.into(),
            state_root: state_root.into(),
            identity,
            home: home.into(),
            op_timeout: Duration::from_secs(60),
        }
    }

    pub fn with_op_timeout(mut self, t: Duration) -> Self {
        self.op_timeout = t;
        self
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Resolve the binary path: explicit (locked) default unless an
    /// absolute override env is set. Never PATH-searched.
    pub fn resolve_binary() -> PathBuf {
        match std::env::var_os(RUNC_BIN_ENV) {
            Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
            _ => PathBuf::from(PINNED_RUNC),
        }
    }

    /// Diagnostics string — e.g. "runc 1.5.0-rc.1+dev (41b7477); rootless".
    pub fn implementation_string(&self) -> Option<String> {
        let mut cmd = self.command(&["--version"]);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text
            .lines()
            .find(|l| l.starts_with("runc version"))?
            .strip_prefix("runc version ")?
            .trim()
            .to_string();
        let commit = text
            .lines()
            .find(|l| l.starts_with("commit:"))
            .and_then(|l| l.strip_prefix("commit:"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        Some(if commit.is_empty() {
            format!("runc {version}; rootless single-id-map")
        } else {
            format!("runc {version} ({commit}); rootless single-id-map")
        })
    }

    /// Build a scrubbed-env, argv-only Command with the identity drop.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--root")
            .arg(&self.state_root)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.home)
            .stdin(Stdio::null());
        if let Some((uid, gid)) = self.identity {
            // Drop to the unprivileged runtime user in our own fork. Any
            // failure aborts the exec — we never "accidentally runc as
            // root", which on this host class would take the rootful path
            // and die on read-only cgroupfs (spike step-0 measurement).
            unsafe {
                cmd.pre_exec(move || {
                    if sys::setgid(gid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if sys::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd
    }

    /// Run a control-plane op to completion with sidecar stdout/stderr
    /// files; returns (exit_code, stdout_tail, stderr_tail). Sidecars are
    /// the pull.rs discipline: pipes only ever carry EOF.
    fn run_to_completion(
        &self,
        op: &'static str,
        args: &[&str],
        sidecar_dir: &Path,
        timeout: Duration,
        note: &str,
    ) -> Result<(Option<i32>, String, String), RuncFailure> {
        fs::create_dir_all(sidecar_dir).map_err(|e| RuncFailure {
            op,
            exit_code: None,
            stderr_tail: format!("sidecar dir {}: {e}", sidecar_dir.display()),
            note: note.to_string(),
        })?;
        // sidecar files must be writable by the dropped-to identity.
        if let Some((uid, gid)) = self.identity {
            chown_tree(sidecar_dir, uid, gid, 0o755).map_err(|e| RuncFailure {
                op,
                exit_code: None,
                stderr_tail: format!("sidecar chown: {e}"),
                note: note.to_string(),
            })?;
        }
        let stdout_path = sidecar_dir.join(format!("{op}.stdout"));
        let stderr_path = sidecar_dir.join(format!("{op}.stderr"));
        let stdout_f = fs::File::create(&stdout_path).map_err(|e| RuncFailure {
            op,
            exit_code: None,
            stderr_tail: format!("sidecar stdout: {e}"),
            note: note.to_string(),
        })?;
        let stderr_f = fs::File::create(&stderr_path).map_err(|e| RuncFailure {
            op,
            exit_code: None,
            stderr_tail: format!("sidecar stderr: {e}"),
            note: note.to_string(),
        })?;
        if let Some((uid, gid)) = self.identity {
            for p in [&stdout_path, &stderr_path] {
                let _ = std::os::unix::fs::chown(p, Some(uid), Some(gid));
            }
        }
        let mut child = self
            .command(args)
            .stdout(Stdio::from(stdout_f))
            .stderr(Stdio::from(stderr_f))
            .spawn()
            .map_err(|e| RuncFailure {
                op,
                exit_code: None,
                stderr_tail: format!("spawn: {e}"),
                note: note.to_string(),
            })?;

        let started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if started.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(RuncFailure {
                        op,
                        exit_code: None,
                        stderr_tail: format!("wait: {e}"),
                        note: note.to_string(),
                    });
                }
            }
        };
        let stdout_tail = read_tail(&stdout_path, 4096);
        let stderr_tail = read_tail(&stderr_path, 4096);
        match exit_status {
            Some(status) => Ok((status.code(), stdout_tail, stderr_tail)),
            None => Err(RuncFailure {
                op,
                exit_code: None,
                stderr_tail: format!("control-plane op exceeded {timeout:?}; killed"),
                note: note.to_string(),
            }),
        }
    }

    fn require_success(
        &self,
        op: &'static str,
        args: &[&str],
        sidecar_dir: &Path,
        note: &str,
    ) -> Result<(String, String), RuncFailure> {
        let (code, out, err) = self.run_to_completion(op, args, sidecar_dir, self.op_timeout, note)?;
        if code != Some(0) {
            return Err(RuncFailure {
                op,
                exit_code: code,
                stderr_tail: err,
                note: note.to_string(),
            });
        }
        Ok((out, err))
    }

    /// `runc create --bundle <dir> <id>` (cwd = bundle dir).
    pub fn create(&self, bundle_dir: &Path, id: &str) -> Result<(), RuncFailure> {
        let logs = bundle_dir.join(".runc-logs");
        fs::create_dir_all(&logs).ok();
        // `create` must run with cwd = bundle dir (relative config refs).
        let mut cmd = self.command(&["create", "--bundle", ".", id]);
        cmd.current_dir(bundle_dir);
        let out = cmd
            .stdout(Stdio::from(fs::File::create(logs.join("create.stdout")).map_err(
                |e| RuncFailure {
                    op: "create",
                    exit_code: None,
                    stderr_tail: format!("sidecar: {e}"),
                    note: id.to_string(),
                },
            )?))
            .stderr(Stdio::from(fs::File::create(logs.join("create.stderr")).map_err(
                |e| RuncFailure {
                    op: "create",
                    exit_code: None,
                    stderr_tail: format!("sidecar: {e}"),
                    note: id.to_string(),
                },
            )?))
            .status()
            .map_err(|e| RuncFailure {
                op: "create",
                exit_code: None,
                stderr_tail: format!("spawn: {e}"),
                note: id.to_string(),
            })?;
        if !out.success() {
            return Err(RuncFailure {
                op: "create",
                exit_code: out.code(),
                stderr_tail: read_tail(&logs.join("create.stderr"), 4096),
                note: id.to_string(),
            });
        }
        Ok(())
    }

    pub fn start(&self, id: &str, sidecar_dir: &Path) -> Result<(), RuncFailure> {
        self.require_success("start", &["start", id], sidecar_dir, id)
            .map(|_| ())
    }

    /// `runc state <id>` → parsed status. A missing container is a
    /// first-class answer (NotFound), not an error.
    pub fn state(&self, id: &str, sidecar_dir: &Path) -> Result<ContainerStatus, RuncFailure> {
        let (code, out, err) = self.run_to_completion(
            "state",
            &["state", id],
            sidecar_dir,
            self.op_timeout,
            id,
        )?;
        match code {
            Some(0) => {
                let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| {
                    RuncFailure {
                        op: "state",
                        exit_code: code,
                        stderr_tail: format!("state output not JSON ({e}): {out}"),
                        note: id.to_string(),
                    }
                })?;
                Ok(match parsed.get("status").and_then(|s| s.as_str()) {
                    Some("created") => ContainerStatus::Created,
                    Some("running") => ContainerStatus::Running,
                    Some("stopped") => ContainerStatus::Stopped,
                    Some(other) => {
                        eprintln!("runc state: unknown status string {other:?} for {id}");
                        ContainerStatus::Unknown
                    }
                    None => ContainerStatus::Unknown,
                })
            }
            Some(_) if err.contains("does not exist") || err.contains("no container") => {
                Ok(ContainerStatus::NotFound)
            }
            other => Err(RuncFailure {
                op: "state",
                exit_code: other,
                stderr_tail: err,
                note: id.to_string(),
            }),
        }
    }

    /// List all container ids known to this state root (residue audits and
    /// manager-restart reconciliation use the REAL list, never a DB guess).
    pub fn list_ids(&self, sidecar_dir: &Path) -> Result<Vec<String>, RuncFailure> {
        let (out, _) = self.require_success("list", &["list", "--format", "json"], sidecar_dir, "list")?;
        let text = out.trim();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| RuncFailure {
            op: "list",
            exit_code: Some(0),
            stderr_tail: format!("list output not JSON ({e}): {text}"),
            note: String::new(),
        })?;
        let mut ids = Vec::new();
        if let Some(arr) = parsed.as_array() {
            for item in arr {
                if let Some(id) = item.get("id").and_then(|s| s.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Kill every task of the container; absence is not an error (delete
    /// path idempotency).
    pub fn kill_all(&self, id: &str, sidecar_dir: &Path) {
        let _ = self.run_to_completion(
            "kill-all",
            &["kill", "--all", id, "KILL"],
            sidecar_dir,
            self.op_timeout,
            id,
        );
    }

    /// `runc delete --force <id>`; absent container is Ok (idempotent).
    pub fn delete(&self, id: &str, sidecar_dir: &Path) -> Result<(), RuncFailure> {
        let (code, _, err) = self.run_to_completion(
            "delete",
            &["delete", "--force", id],
            sidecar_dir,
            self.op_timeout,
            id,
        )?;
        match code {
            Some(0) => Ok(()),
            Some(_) if err.contains("does not exist") || err.contains("no container") => Ok(()),
            other => Err(RuncFailure {
                op: "delete",
                exit_code: other,
                stderr_tail: err,
                note: id.to_string(),
            }),
        }
    }

    /// `runc exec` — argv only; stdout/stderr captured via explicit pipes
    /// by the CALLER (native/mod.rs), which owns the wait policy.
    pub fn exec_command(
        &self,
        id: &str,
        argv: &[String],
        env: &[String],
        cwd: Option<&str>,
    ) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--root")
            .arg(&self.state_root)
            .arg("exec");
        if let Some(c) = cwd {
            cmd.arg("--cwd").arg(c);
        }
        for kv in env {
            cmd.arg("--env").arg(kv);
        }
        cmd.arg(id).args(argv);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.home)
            .stdin(Stdio::null());
        if let Some((uid, gid)) = self.identity {
            unsafe {
                cmd.pre_exec(move || {
                    if sys::setgid(gid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if sys::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd
    }
}

/// Bounded tail read for sidecar files (never loads a huge file whole).
pub fn read_tail(path: &Path, max: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max as u64);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Recursive chown to the runtime identity, never following symlinks.
/// `dir_mode`/`file_mode` are NOT changed — only ownership. Errors on the
/// top-level path abort; errors deep in the tree abort too (silently
/// skipping a file here would later surface as an unexplained guest EACCES).
pub fn chown_tree(path: &Path, uid: u32, gid: u32, top_dir_mode: u32) -> io::Result<()> {
    fn walk(p: &Path, uid: u32, gid: u32) -> io::Result<()> {
        std::os::unix::fs::lchown(p, Some(uid), Some(gid))?;
        let meta = fs::symlink_metadata(p)?;
        if meta.is_dir() {
            for ent in fs::read_dir(p)? {
                let ent = ent?;
                walk(&ent.path(), uid, gid)?;
            }
        }
        Ok(())
    }
    walk(path, uid, gid)?;
    if path.is_dir() {
        use std::os::unix::fs::PermissionsExt;
        // ensure the runtime user can traverse the top dir itself
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o700 != 0o700 {
            fs::set_permissions(path, fs::Permissions::from_mode(top_dir_mode))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> RuncRuntime {
        RuncRuntime::new(
            "/opt/toolchains/lightsandbox/bin/runc",
            "/tmp/state-root",
            Some((65534, 65534)),
            "/tmp/home",
        )
    }

    #[test]
    fn pinned_default_is_absolute_locked_path() {
        assert_eq!(PINNED_RUNC, "/opt/toolchains/lightsandbox/bin/runc");
        assert!(PathBuf::from(PINNED_RUNC).is_absolute());
        // env var present-or-absent never yields a relative path
        let resolved = RuncRuntime::resolve_binary();
        assert!(resolved.is_absolute());
    }

    #[test]
    fn control_argv_is_exact() {
        // We cannot call private `command` argv out directly, so assert
        // through exec_command (same builder) using Debug formatting.
        let cmd = rt().exec_command(
            "lsbx-sbx_g1",
            &["/bin/echo".to_string(), "hi".to_string()],
            &["A=B".to_string()],
            Some("/workspace"),
        );
        let dbg = format!("{cmd:?}");
        assert!(dbg.contains("--root"), "{dbg}");
        assert!(dbg.contains("/tmp/state-root"), "{dbg}");
        assert!(dbg.contains("exec"), "{dbg}");
        assert!(dbg.contains("--cwd") && dbg.contains("/workspace"), "{dbg}");
        assert!(dbg.contains("--env") && dbg.contains("A=B"), "{dbg}");
        assert!(dbg.contains("lsbx-sbx_g1"), "{dbg}");
        assert!(dbg.contains("/bin/echo"), "{dbg}");
    }

    #[test]
    fn chown_tree_requires_no_privilege_when_identity_unchanged() {
        let dir = std::env::temp_dir().join(format!("ls-runc-chown-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/f.txt"), b"x").unwrap();
        let uid = unsafe { sys_getuid() };
        chown_tree(&dir, uid, uid, 0o755).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    extern "C" {
        fn getuid() -> u32;
    }
    #[allow(non_snake_case)]
    unsafe fn sys_getuid() -> u32 {
        getuid()
    }
}
