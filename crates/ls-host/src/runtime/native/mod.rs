//! `NativeRuncBackend`: the BackendKind::Native implementation of
//! [`SandboxBackend`] (ADR 0002 §7). Everything binary-specific lives
//! behind this module; the manager only ever sees the trait.
//!
//! Per-sandbox effects (the ONLY blast radius of this backend):
//! * the per-sandbox writable lease rootfs (owned by the manager/lease);
//! * the guest init + receipt inside that rootfs (`/.lsbx/`);
//! * one OCI bundle dir `<native_root>/bundles/<label>/`;
//! * one container registration under the runc state root.
//!
//! Create is journaled: bundle write -> runc create run LIFO-rollback on
//! any failure, so a failed create leaves zero backend-owned residue
//! (T08-C03).

pub mod bundle;
pub mod runc;

use crate::runtime::backend::{
    BackendError, BackendExecOutcome, BackendExecSpec, BackendInstance, BackendSandboxSpec,
    BackendState, ExecTermination, InitReceipt, InitReceiptTermination, SandboxBackend,
};
use crate::runtime::journal::{Journal, RolledBack, StepOutcome};
use ls_core::contracts::Status;
use ls_core::domain::backend::{dim, BackendCapabilities, BackendKind};
use std::collections::BTreeMap;
use runc::{ContainerStatus, RuncFailure, RuncRuntime};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Native backend configuration. Absolute paths only; identity is the
/// unprivileged runtime user the runc process runs as.
#[derive(Debug, Clone)]
pub struct NativeBackendConfig {
    pub runc: RuncRuntime,
    /// Host path of the `lsguest-init` binary (injected per sandbox).
    pub guest_init: PathBuf,
    /// `<native_root>`; bundles live in `<native_root>/bundles/<label>`.
    pub native_root: PathBuf,
    /// The uid/gid the guest root maps to AND the runc process identity
    /// (single-ID map: container uid 0 == this host uid).
    pub identity: (u32, u32),
}

#[derive(Clone)]
pub struct NativeRuncBackend {
    config: NativeBackendConfig,
    implementation: Option<String>,
}

/// Durable label: `lsbx-<sandbox_id>-g<generation>`. Charset stays inside
/// the sandbox-id alphabet plus `-`, so every id-validated string is a
/// valid container id for the runtime.
pub fn instance_label(sandbox_id: &str, generation: u64) -> String {
    format!("lsbx-{sandbox_id}-g{generation}")
}

impl NativeRuncBackend {
    pub fn new(config: NativeBackendConfig) -> Result<Self, BackendError> {
        if !config.guest_init.is_file() {
            return Err(BackendError::new(
                "backend-open",
                format!(
                    "guest init binary missing: {} (build ls-guest first)",
                    config.guest_init.display()
                ),
            ));
        }
        if !config.runc.binary().is_file() {
            return Err(BackendError::new(
                "backend-open",
                format!(
                    "runtime binary missing: {} (pinned in specs/upstream-lock.json)",
                    config.runc.binary().display()
                ),
            ));
        }
        fs::create_dir_all(config.native_root.join("bundles"))
            .map_err(|e| BackendError::new("backend-open", format!("native_root: {e}")))?;
        fs::create_dir_all(config.runc.state_root())
            .map_err(|e| BackendError::new("backend-open", format!("state_root: {e}")))?;
        // The runc state root must be traversable+writable by the identity.
        let (uid, gid) = config.identity;
        runc::chown_tree(config.runc.state_root(), uid, gid, 0o700)
            .map_err(|e| BackendError::new("backend-open", format!("state_root chown: {e}")))?;
        runc::chown_tree(&config.native_root.join("bundles"), uid, gid, 0o755)
            .map_err(|e| BackendError::new("backend-open", format!("bundles chown: {e}")))?;
        let implementation = config.runc.implementation_string();
        if implementation.is_none() {
            return Err(BackendError::new(
                "backend-open",
                format!(
                    "runtime binary at {} did not answer --version",
                    config.runc.binary().display()
                ),
            ));
        }
        Ok(NativeRuncBackend {
            config,
            implementation,
        })
    }

    pub fn config(&self) -> &NativeBackendConfig {
        &self.config
    }

    fn bundle_dir(&self, label: &str) -> PathBuf {
        self.config.native_root.join("bundles").join(label)
    }

    /// runc stderr sidecar dir for one instance (falls back to state_root
    /// when the bundle dir is gone — e.g. state queries after deletion).
    fn sidecar_dir(&self, label: &str) -> PathBuf {
        let dir = self.bundle_dir(label).join(".runc-logs");
        if self.bundle_dir(label).is_dir() {
            dir
        } else {
            self.config.runc.state_root().join(".sidecars").join(label)
        }
    }

    /// The lease rootfs path of an existing instance, read from its own
    /// bundle config (single source of truth — restart-safe).
    fn instance_rootfs(&self, instance: &BackendInstance) -> Result<PathBuf, BackendError> {
        let cfg_path = self.bundle_dir(&instance.label).join("config.json");
        let text = fs::read_to_string(&cfg_path).map_err(|e| {
            BackendError::new(
                "instance-inspect",
                format!("bundle config unreadable ({}): {e}", cfg_path.display()),
            )
        })?;
        let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            BackendError::new("instance-inspect", format!("bundle config invalid: {e}"))
        })?;
        parsed
            .get("root")
            .and_then(|r| r.get("path"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .ok_or_else(|| BackendError::new("instance-inspect", "bundle config missing root.path"))
    }

    fn runc_err(stage: &'static str, e: RuncFailure) -> BackendError {
        BackendError::new(stage, e.to_string())
    }
}

impl SandboxBackend for NativeRuncBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Native
    }

    fn implementation(&self) -> Option<String> {
        self.implementation.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Measured facts of THIS port of the Native backend (ADR 0001 /
        // spike evidence on the 5.15 PORTABLE_ROOTLESS host class):
        // * multi_uid: single-ID map only (no newuidmap) -> UNSUPPORTED;
        // * private_proc: /proc is rbind-ro of the host proc (adr A1) ->
        //   UNSUPPORTED (never claimed);
        // * full_init_system: never claimed for Native;
        // * hard_memory_limit: rootless has no cgroup delegation -> UNSUPPORTED;
        // * seccomp: v1 bundle emits no seccomp filter -> UNSUPPORTED;
        // * outbound_network / net_admin: private netns is loopback-only
        //   (net=none) -> UNSUPPORTED.
        // A later task upgrades dimensions as mechanisms land; nothing here
        // is a promise — every value is the current observation.
        let dims: BTreeMap<String, Status> = dim::ALL
            .iter()
            .map(|d| (d.to_string(), Status::Unsupported))
            .collect();
        BackendCapabilities {
            kind: BackendKind::Native,
            implementation: self.implementation.clone(),
            dimensions: dims,
            available: true,
            note: "spike-measured rootless Native offer v1 (single-id map, no seccomp, net=none)"
                .to_string(),
        }
    }

    fn handle(&self, label: &str) -> BackendInstance {
        BackendInstance {
            label: label.to_string(),
        }
    }

    fn create(&self, spec: &BackendSandboxSpec) -> Result<BackendInstance, BackendError> {
        let label = instance_label(spec.sandbox_id.as_str(), spec.generation);
        let bundle_dir = self.bundle_dir(&label);
        let (uid, gid) = self.config.identity;
        let mut j = Journal::new();

        // step 1: inject the tiny init into the lease rootfs (compensation:
        // remove the injected tree — lease release of the rootfs itself is
        // the manager's step, never ours).
        let init_dest_rootfs = spec.rootfs.clone();
        let init_src = self.config.guest_init.clone();
        j.step("inject-init", move || {
            let dest = bundle::inject_init(&init_dest_rootfs, &init_src)?;
            // The whole /.lsbx dir — not just the init file — must belong
            // to the runtime identity: guest uid 0 (== identity) writes
            // the receipt INTO this dir. Chowning only the file leaves a
            // root-owned dir and receipt writes die with EACCES (real
            // evidence, T08-C02 first run).
            let lsbx_dir = dest
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| dest.clone());
            runc::chown_tree(&lsbx_dir, uid, gid, 0o755)
                .map_err(|e| BackendError::new("inject-init", format!("chown: {e}")))?;
            let undo_dir = init_dest_rootfs.join(".lsbx");
            Ok(StepOutcome::with_compensation(
                Some(dest.display().to_string()),
                move || {
                    fs::remove_dir_all(&undo_dir)
                        .map_err(|e| format!("remove injected init {}: {e}", undo_dir.display()))
                },
            ))
        })
        .map_err(Self::rollback_to_error)?;

        // step 2: write the bundle config (compensation: remove bundle dir).
        let cfg = bundle::render_config(spec, &label, self.config.identity);
        let bd = bundle_dir.clone();
        j.step("write-bundle", move || {
            if bd.exists() {
                // a committed bundle for THIS label survives: creating over
                // it would silently alias another owner's files. Refuse.
                return Err(BackendError::new(
                    "write-bundle",
                    format!("bundle dir already exists: {}", bd.display()),
                ));
            }
            bundle::write_bundle(&bd, &cfg)?;
            runc::chown_tree(&bd, uid, gid, 0o755)
                .map_err(|e| BackendError::new("write-bundle", format!("chown: {e}")))?;
            let undo = bd.clone();
            Ok(StepOutcome::with_compensation(
                Some(bd.display().to_string()),
                move || {
                    fs::remove_dir_all(&undo)
                        .map_err(|e| format!("remove bundle {}: {e}", undo.display()))
                },
            ))
        })
        .map_err(Self::rollback_to_error)?;

        // step 3: register the container (compensation: runc delete).
        j.step("runc-create", || {
            let bd = bundle_dir.clone();
            let id = label.clone();
            self.config
                .runc
                .create(&bd, &id)
                .map_err(|e| Self::runc_err("runc-create", e))?;
            let sidecar = self.sidecar_dir(&label);
            let runc2 = self.config.runc.clone();
            let id2 = label.clone();
            Ok(StepOutcome::with_compensation(
                Some(format!("container {id} registered")),
                move || runc2
                    .delete(&id2, &sidecar)
                    .map_err(|e| format!("rollback runc delete of {id2}: {e}")),
            ))
        })
        .map_err(Self::rollback_to_error)?;

        j.commit();
        Ok(BackendInstance { label })
    }

    fn start(&self, instance: &BackendInstance) -> Result<(), BackendError> {
        self.config
            .runc
            .start(&instance.label, &self.sidecar_dir(&instance.label))
            .map_err(|e| Self::runc_err("runc-start", e))
    }

    fn state(&self, instance: &BackendInstance) -> Result<BackendState, BackendError> {
        let status = self
            .config
            .runc
            .state(&instance.label, &self.sidecar_dir(&instance.label))
            .map_err(|e| Self::runc_err("runc-state", e))?;
        Ok(match status {
            ContainerStatus::Created => BackendState::Created,
            ContainerStatus::Running => BackendState::Running,
            ContainerStatus::Stopped => BackendState::Stopped,
            ContainerStatus::NotFound => BackendState::NotFound,
            ContainerStatus::Unknown => BackendState::Unknown,
        })
    }

    fn exec(
        &self,
        instance: &BackendInstance,
        exec: &BackendExecSpec,
    ) -> Result<BackendExecOutcome, BackendError> {
        const CAP: usize = 1024 * 1024;
        let mut cmd = self.config.runc.exec_command(
            &instance.label,
            &exec.argv,
            &exec.env,
            exec.cwd.as_ref().map(|c| c.as_str()),
        );
        let started = Instant::now();
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BackendError::new("exec-spawn", format!("{e}")))?;

        // Bounded readers: CAP bytes retained, the rest drained (never a
        // pipe-fill deadlock) and honestly flagged as truncated.
        fn bounded_reader(
            mut r: impl io::Read + Send + 'static,
        ) -> std::thread::JoinHandle<(String, bool)> {
            std::thread::spawn(move || {
                let mut kept: Vec<u8> = Vec::new();
                let mut buf = [0u8; 8192];
                let mut truncated = false;
                loop {
                    match r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if kept.len() < CAP {
                                let take = (CAP - kept.len()).min(n);
                                kept.extend_from_slice(&buf[..take]);
                                if take < n {
                                    truncated = true;
                                }
                            } else {
                                truncated = true;
                            }
                        }
                        Err(_) => break,
                    }
                }
                (String::from_utf8_lossy(&kept).to_string(), truncated)
            })
        }
        let out_h = child.stdout.take().map(bounded_reader);
        let err_h = child.stderr.take().map(bounded_reader);

        let timed_out;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    timed_out = false;
                    break;
                }
                Ok(None) => {
                    if let Some(budget) = exec.timeout {
                        if started.elapsed() > budget {
                            let _ = child.kill();
                            let _ = child.wait();
                            timed_out = true;
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(BackendError::new("exec-wait", format!("{e}")));
                }
            }
        }
        let (stdout, t1) = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        let (stderr, t2) = err_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        let wall_ms = started.elapsed().as_millis() as u64;

        if timed_out {
            return Ok(BackendExecOutcome {
                termination: ExecTermination::Timeout,
                stdout,
                stderr,
                wall_ms,
                truncated: t1 || t2,
            });
        }
        // The holder's exit code relays the exec'd process's wait status
        // (the holder IS the wait-observing path for this exec).
        let code = child_exit_code(&mut child);
        Ok(BackendExecOutcome {
            termination: ExecTermination::Exited(code),
            stdout,
            stderr,
            wall_ms,
            truncated: t1 || t2,
        })
    }

    fn init_receipt(&self, instance: &BackendInstance) -> Result<Option<InitReceipt>, BackendError> {
        let rootfs = match self.instance_rootfs(instance) {
            Ok(p) => p,
            Err(_) => return Ok(None), // bundle gone: receipt is irrecoverable
        };
        let path = rootfs.join(".lsbx/receipt.json");
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path).map_err(|e| {
            BackendError::new("receipt-read", format!("{}: {e}", path.display()))
        })?;
        parse_receipt(&text).map(Some).ok_or_else(|| {
            BackendError::new(
                "receipt-read",
                format!("receipt at {} is malformed: {text}", path.display()),
            )
        })
    }

    fn delete(&self, instance: &BackendInstance) -> Result<(), BackendError> {
        let sidecar = self.sidecar_dir(&instance.label);
        self.config.runc.kill_all(&instance.label, &sidecar);
        self.config
            .runc
            .delete(&instance.label, &sidecar)
            .map_err(|e| Self::runc_err("runc-delete", e))?;
        let bd = self.bundle_dir(&instance.label);
        if bd.exists() {
            fs::remove_dir_all(&bd).map_err(|e| {
                BackendError::new(
                    "bundle-remove",
                    format!("remove bundle {}: {e}", bd.display()),
                )
            })?;
        }
        Ok(())
    }

    fn residue_paths(&self, instance: &BackendInstance) -> Vec<PathBuf> {
        let mut out = vec![self.bundle_dir(&instance.label)];
        if let Ok(rootfs) = self.instance_rootfs(instance) {
            out.push(rootfs.join(".lsbx"));
        }
        out
    }

    fn enumerate_live(&self) -> Vec<String> {
        let sidecar = self.config.native_root.join("bundles").join(".list-sidecar");
        self.config
            .runc
            .list_ids(&sidecar)
            .unwrap_or_else(|e| {
                // Enumeration is reconcile-surface only; never fake a list.
                eprintln!("WARNING: backend enumerate_live failed: {e}");
                Vec::new()
            })
    }
}

impl NativeRuncBackend {
    fn rollback_to_error(rb: RolledBack<BackendError>) -> BackendError {
        rb.original.with_cleanup(
            rb.compensation_errors
                .iter()
                .map(|(step, err)| format!("{step}: {err}"))
                .collect(),
        )
    }
}

/// Exit code of an already-reaped child (we always try_wait-ed Some first).
fn child_exit_code(child: &mut std::process::Child) -> i32 {
    match child.try_wait() {
        Ok(Some(status)) => status.code().unwrap_or(-1),
        _ => -1,
    }
}

/// Strict parse of the guest init receipt (internal host↔guest surface).
pub fn parse_receipt(text: &str) -> Option<InitReceipt> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("version")?.as_u64()? != 1 {
        return None;
    }
    let sandbox_id = v.get("sandbox_id")?.as_str()?.to_string();
    let child_pid = v.get("child_pid").and_then(|p| p.as_i64());
    let t = v.get("termination")?;
    let kind = t.get("kind")?.as_str()?;
    let termination = match kind {
        "EXITED" => InitReceiptTermination::Exited {
            exit_code: t.get("exit_code")?.as_i64()?,
        },
        "SIGNALED" => InitReceiptTermination::Signaled {
            signal: t.get("signal")?.as_i64()?,
        },
        "SPAWN_FAILED" => InitReceiptTermination::SpawnFailed {
            detail: t
                .get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
        },
        _ => return None,
    };
    Some(InitReceipt {
        sandbox_id,
        child_pid,
        termination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_stay_in_id_charset() {
        assert_eq!(instance_label("sbx_01.A-b", 3), "lsbx-sbx_01.A-b-g3");
        // charset compatible with runc container ids and SandboxId itself
        assert!(ls_core::domain::ids::SandboxId::new(instance_label("sbx_01", 1)).is_ok());
    }

    #[test]
    fn receipt_parse_roundtrip() {
        let body = "{\"version\":1,\"sandbox_id\":\"sbx_01234567\",\"child_pid\":421,\
\"termination\":{\"kind\":\"EXITED\",\"exit_code\":7},\"source\":\"init_waitpid\"}\n";
        let r = parse_receipt(body).unwrap();
        assert_eq!(r.sandbox_id, "sbx_01234567");
        assert_eq!(r.child_pid, Some(421));
        assert_eq!(
            r.termination,
            InitReceiptTermination::Exited { exit_code: 7 }
        );

        let sig = "{\"version\":1,\"sandbox_id\":\"s\",\"child_pid\":1,\
\"termination\":{\"kind\":\"SIGNALED\",\"signal\":15},\"source\":\"init_waitpid\"}";
        assert!(matches!(
            parse_receipt(sig).unwrap().termination,
            InitReceiptTermination::Signaled { signal: 15 }
        ));

        let failed = "{\"version\":1,\"sandbox_id\":\"s\",\"child_pid\":null,\
\"termination\":{\"kind\":\"SPAWN_FAILED\",\"detail\":\"ENOENT\"},\"source\":\"init_spawn\"}";
        assert!(matches!(
            parse_receipt(failed).unwrap().termination,
            InitReceiptTermination::SpawnFailed { .. }
        ));
        // malformed never parses as success
        assert!(parse_receipt("{}").is_none());
        assert!(parse_receipt("not json").is_none());
    }
}
