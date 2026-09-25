//! OCI bundle construction for the Native backend.
//!
//! The emitted `config.json` follows EXACTLY the measured minimal fixture
//! of spike/lab.py (`minimal_config`, gate: "this exact minimal
//! net=none/none-fixture passes as nobody on 5.15"):
//! * namespaces: user, pid, mount, ipc, uts, network (6; network is a
//!   private namespace with loopback only — net=none);
//! * single-ID uid/gid mappings (container 0 -> runtime identity, size 1;
//!   no newuidmap exists on this host);
//! * minimal caps: CHOWN, DAC_OVERRIDE, FOWNER, FSETID, KILL, MKNOD,
//!   SETGID, SETUID, SYS_CHROOT, SETPCAP in bounding/effective/permitted,
//!   ambient empty;
//! * noNewPrivileges: true; RLIMIT_NOFILE 1024/1024;
//! * mounts: tmpfs /dev (nosuid,64k), /proc as rbind of host /proc
//!   read-only (adr/0001 A1 open default — honest consequence: the
//!   `private_proc` dimension is NOT claimed by this v1 backend offer);
//! * absolute rootfs path = the sandbox's writable lease root; writable.
//!
//! PID 1 in the guest is always `lsguest-init` (injected into the lease
//! rootfs at `/.lsbx/init`), wrapping the image entrypoint argv. The image
//! entrypoint therefore runs as a direct child of a reaping,
//! signal-forwarding init — never as an orphan-reaping shell.

use crate::runtime::backend::{BackendError, BackendSandboxSpec};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Guest-side locations injected into the sandbox rootfs.
pub const GUEST_INIT_PATH: &str = "/.lsbx/init";
pub const GUEST_RECEIPT_PATH: &str = "/.lsbx/receipt.json";

/// The measured minimal capability set (spike/lab.py MINIMAL_CAPS).
pub const MINIMAL_CAPS: [&str; 10] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_MKNOD",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SYS_CHROOT",
    "CAP_SETPCAP",
];

pub const NAMESPACE_TYPES: [&str; 6] = ["user", "pid", "mount", "ipc", "uts", "network"];

/// Render the exact `config.json` for one sandbox spec.
pub fn render_config(
    spec: &BackendSandboxSpec,
    instance_label: &str,
    identity: (u32, u32),
) -> Value {
    // PID1 = tiny init wrapping the image entrypoint.
    let mut args: Vec<Value> = vec![
        json!(GUEST_INIT_PATH),
        json!("--sandbox-id"),
        json!(spec.sandbox_id.as_str()),
        json!("--receipt"),
        json!(GUEST_RECEIPT_PATH),
        json!("--"),
    ];
    args.extend(spec.argv.iter().map(|a| json!(a)));
    let caps: Vec<Value> = MINIMAL_CAPS.iter().map(|c| json!(c)).collect();
    let (host_uid, host_gid) = identity;
    json!({
        "ociVersion": "1.3.0",
        "process": {
            "terminal": false,
            "user": { "uid": spec.user.0, "gid": spec.user.1 },
            "args": args,
            "env": spec.env,
            "cwd": spec.cwd.as_str(),
            "capabilities": {
                "bounding": caps,
                "effective": caps,
                "permitted": caps,
                "ambient": [],
            },
            "noNewPrivileges": true,
            "rlimits": [{ "type": "RLIMIT_NOFILE", "hard": 1024, "soft": 1024 }],
        },
        "root": { "path": spec.rootfs, "readonly": false },
        "hostname": spec.hostname,
        "mounts": [
            {
                "destination": "/dev",
                "type": "tmpfs",
                "source": "tmpfs",
                "options": ["nosuid", "strictatime", "mode=755", "size=65536k"],
            },
            {
                // adr/0001 A1: host /proc rbind read-only (the measured
                // form on 5.15). Consequence honestly reported in the
                // backend offer: private_proc UNSUPPORTED in v1.
                "destination": "/proc",
                "type": "bind",
                "source": "/proc",
                "options": ["rbind", "ro", "nosuid", "noexec", "nodev"],
            },
        ],
        "linux": {
            "uidMappings": [{ "containerID": 0, "hostID": host_uid, "size": 1 }],
            "gidMappings": [{ "containerID": 0, "hostID": host_gid, "size": 1 }],
            "namespaces": NAMESPACE_TYPES.iter().map(|t| json!({ "type": t })).collect::<Vec<_>>(),
        },
        "annotations": {
            // cross-reference for residue audits + restart reconciliation;
            // never parsed for security decisions.
            "org.lightsandbox.instance": instance_label,
        },
    })
}

/// Inject the tiny init into the leased rootfs. Idempotent per content:
/// we always rewrite (the lease is single-owner per sandbox).
pub fn inject_init(rootfs: &Path, init_binary: &Path) -> Result<PathBuf, BackendError> {
    let dir = rootfs.join(".lsbx");
    fs::create_dir_all(&dir).map_err(|e| BackendError::new("init-inject", format!("mkdir {}: {e}", dir.display())))?;
    let dest = rootfs.join(GUEST_INIT_PATH.trim_start_matches('/'));
    stream_copy_executable(init_binary, &dest)
        .map_err(|e| BackendError::new("init-inject", format!("copy {} -> {}: {e}", init_binary.display(), dest.display())))?;
    Ok(dest)
}

/// Real-userspace copy + 0755 (never fs::copy: clone-offload extent-sharing
/// semantics are host-fs-class dependent; a plain read/write is exact).
fn stream_copy_executable(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    let mut r = fs::File::open(src)?;
    let mut w = fs::File::create(dst)?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
    }
    w.sync_all()?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Write the bundle: config.json only. The rootfs is the lease root
/// (absolute path in config) — the bundle dir holds no second rootfs.
pub fn write_bundle(bundle_dir: &Path, config: &Value) -> Result<(), BackendError> {
    fs::create_dir_all(bundle_dir)
        .map_err(|e| BackendError::new("bundle", format!("mkdir {}: {e}", bundle_dir.display())))?;
    let body = serde_json::to_vec_pretty(config)
        .map_err(|e| BackendError::new("bundle", format!("serialize config: {e}")))?;
    let tmp = bundle_dir.join(".config.json.tmp");
    fs::write(&tmp, &body).map_err(|e| BackendError::new("bundle", format!("write tmp: {e}")))?;
    fs::rename(&tmp, bundle_dir.join("config.json"))
        .map_err(|e| BackendError::new("bundle", format!("publish config.json: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_core::domain::ids::{GuestPath, SandboxId};

    fn spec() -> BackendSandboxSpec {
        BackendSandboxSpec {
            sandbox_id: SandboxId::new("sbx_test01").unwrap(),
            generation: 1,
            rootfs: PathBuf::from("/data/rootfs/sandbox-sbx_test01/rootfs"),
            hostname: "sbx-test01".into(),
            argv: vec!["/bin/sleep".into(), "600".into()],
            env: vec!["PATH=/usr/bin:/bin".into(), "A=1".into()],
            cwd: GuestPath::new("/workspace").unwrap(),
            user: (0, 0),
        }
    }

    #[test]
    fn config_matches_measured_rootless_shape() {
        let cfg = render_config(&spec(), "lsbx-sbx_test01-g1", (65534, 65534));
        // namespaces: exactly the measured 6
        let ns: Vec<&str> = cfg["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(ns, vec!["user", "pid", "mount", "ipc", "uts", "network"]);
        // single-ID mappings only (no newuidmap class)
        assert_eq!(
            cfg["linux"]["uidMappings"],
            json!([{ "containerID": 0, "hostID": 65534, "size": 1 }])
        );
        assert_eq!(
            cfg["linux"]["gidMappings"],
            json!([{ "containerID": 0, "hostID": 65534, "size": 1 }])
        );
        // caps = the measured minimal list in all three sets, ambient empty
        for set in ["bounding", "effective", "permitted"] {
            let list: Vec<&str> = cfg["process"]["capabilities"][set]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap())
                .collect();
            assert_eq!(list, MINIMAL_CAPS, "{set}");
        }
        assert_eq!(
            cfg["process"]["capabilities"]["ambient"],
            json!([])
        );
        assert_eq!(cfg["process"]["noNewPrivileges"], json!(true));
        // mounts: tmpfs /dev + rbind-ro /proc; nothing else
        let mounts = cfg["mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0]["destination"], json!("/dev"));
        assert_eq!(mounts[0]["type"], json!("tmpfs"));
        assert!(mounts[0]["options"]
            .as_array()
            .unwrap()
            .contains(&json!("nosuid")));
        assert_eq!(mounts[1]["destination"], json!("/proc"));
        assert_eq!(mounts[1]["type"], json!("bind"));
        assert!(mounts[1]["options"].as_array().unwrap().contains(&json!("ro")));
        assert!(mounts[1]["options"].as_array().unwrap().contains(&json!("rbind")));
        // NO cgroup section (rootless cannot delegate; omit entirely)
        assert!(cfg["linux"].get("cgroupsPath").is_none());
        assert!(cfg["linux"].get("resources").is_none());
        // NO seccomp block in v1 (and the offer says UNSUPPORTED)
        assert!(cfg["linux"].get("seccomp").is_none());
    }

    #[test]
    fn pid1_is_always_the_tiny_init_wrapping_entrypoint() {
        let cfg = render_config(&spec(), "lsbx-sbx_test01-g1", (65534, 65534));
        let args: Vec<&str> = cfg["process"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec![
                "/.lsbx/init",
                "--sandbox-id",
                "sbx_test01",
                "--receipt",
                "/.lsbx/receipt.json",
                "--",
                "/bin/sleep",
                "600"
            ]
        );
        assert_eq!(cfg["process"]["cwd"], json!("/workspace"));
        assert_eq!(cfg["process"]["env"], json!(["PATH=/usr/bin:/bin", "A=1"]));
        assert_eq!(cfg["process"]["user"], json!({"uid":0,"gid":0}));
        assert_eq!(cfg["hostname"], json!("sbx-test01"));
        assert_eq!(cfg["root"], json!({"path":"/data/rootfs/sandbox-sbx_test01/rootfs","readonly":false}));
    }

    #[test]
    fn init_injection_produces_executable_guest_path() {
        let rootfs = std::env::temp_dir().join(format!("ls-bundle-inj-{}", std::process::id()));
        let _ = fs::remove_dir_all(&rootfs);
        fs::create_dir_all(&rootfs).unwrap();
        let src = rootfs.join("init.src");
        fs::write(&src, b"#!/bin/true-binary\n").unwrap();
        let dest = inject_init(&rootfs, &src).unwrap();
        assert!(dest.ends_with(".lsbx/init"));
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "injected init must be executable");
        assert_eq!(fs::read(&dest).unwrap(), b"#!/bin/true-binary\n");
        let _ = fs::remove_dir_all(&rootfs);
    }
}
