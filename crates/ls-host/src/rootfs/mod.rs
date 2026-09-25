//! T07 — safe rootfs preparation, per-sandbox writable layers, lease-driven GC.
//!
//! Boundaries (plans/tasks/T07.md):
//! * layer application (whiteout / override / symlink / hardlink semantics,
//!   path-escape rejection) belongs to the pinned mature unpacker (umoci) —
//!   this module never implements its own tar extraction that could follow
//!   hostile links (docs/v5/09_RUNTIME_DETAILS.md §17);
//! * a prepared rootfs is addressable ONLY via a [`RootfsLease`]; every
//!   sandbox gets its OWN writable layer (workspace-writable-only is not a
//!   substitute for a fully writable rootfs);
//! * GC owns two classes: stale incomplete debris AND stale committed
//!   leases with dead owners (inactive + cutoff-expired). It audits
//!   /proc/self/mountinfo and REFUSES silent deletion when a mount
//!   reference exists under the data root;
//! * sharing vs full-copy memory cost is reported honestly (review focus:
//!   reflink sharing has a real cost advantage, Docker's same-inode sharing
//!   has it too — FULL_COPY does not).

pub mod lease;
pub mod prepare;

pub use lease::{GcReport, RootfsLease};
pub use prepare::{Driver, PrepareReport, RootfsManager};

use crate::images::error::ImageError;
use std::path::PathBuf;

/// Rootfs-layer errors re-use the image error surface (codes stay contract-
/// stable); `ImageError::Io` covers fs failures, `LayoutInvalid` covers
/// semantic violations found by our post-guards.
pub type Result<T> = std::result::Result<T, ImageError>;

pub fn layout_invalid(detail: impl Into<String>) -> ImageError {
    ImageError::LayoutInvalid {
        detail: detail.into(),
    }
}

/// Everything a prepared, leased rootfs wants to say about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsHandle {
    /// unique lease id (uuid-ish; kernel pid + counter + nanos)
    pub lease_id: String,
    /// the immutable base this was prepared from (manifest digest hex)
    pub base_digest: String,
    pub driver: Driver,
    /// writable root path for exactly this sandbox
    pub root: PathBuf,
}

#[cfg(test)]
pub(crate) mod testkit {
    //! Fixture builders: hand-rolled USTAR writer (FIXTURE INPUT ONLY —
    //! extraction is always the pinned unpacker's job), OCI layout builder
    //! with real tar layers, and helpers for the GC/mount tests.
    use ls_core::domain::idempotency::sha256_hex;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("ls-t07-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    // ------------------------------------------------------------ ustar out

    #[derive(Clone)]
    pub enum Ent {
        File { path: String, mode: u32, data: Vec<u8> },
        Dir { path: String, mode: u32 },
        Symlink { path: String, target: String },
        Hardlink { path: String, target: String },
    }

    fn octal(v: u64, width: usize) -> Vec<u8> {
        let s = format!("{:0width$o}", v, width = width - 1);
        let mut b = s.into_bytes();
        b.push(0);
        b
    }

    fn header(name: &str, mode: u32, size: u64, typeflag: u8, linkname: &str) -> Vec<u8> {
        assert!(name.len() <= 100, "fixture tar name too long: {name}");
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(&octal(mode as u64, 8));
        h[108..116].copy_from_slice(&octal(0, 8)); // uid
        h[116..124].copy_from_slice(&octal(0, 8)); // gid
        h[124..136].copy_from_slice(&octal(size, 12));
        h[136..148].copy_from_slice(&octal(1_700_000_000, 12)); // fixed mtime
        h[156] = typeflag;
        h[157..157 + linkname.len()].copy_from_slice(linkname.as_bytes());
        h[257..262].copy_from_slice(b"ustar");
        h[263..265].copy_from_slice(b"00");
        h[265..270].copy_from_slice(b"root\0");
        h[297..302].copy_from_slice(b"root\0");
        // checksum: field filled with spaces during computation
        for b in &mut h[148..156] {
            *b = b' ';
        }
        let sum: u64 = h.iter().map(|b| *b as u64).sum();
        let chk = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(chk.as_bytes());
        h
    }

    /// Serialize entries as a ustar tar (fixtures only).
    pub fn tar(entries: &[Ent]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in entries {
            match e {
                Ent::File { path, mode, data } => {
                    out.extend(header(path, *mode, data.len() as u64, b'0', ""));
                    out.extend(data);
                    let pad = (512 - (data.len() % 512)) % 512;
                    out.extend(vec![0u8; pad]);
                }
                Ent::Dir { path, mode } => {
                    let p = format!("{}/", path.trim_end_matches('/'));
                    out.extend(header(&p, *mode, 0, b'5', ""));
                }
                Ent::Symlink { path, target } => {
                    out.extend(header(path, 0o777, 0, b'2', target));
                }
                Ent::Hardlink { path, target } => {
                    out.extend(header(path, 0o644, 0, b'1', target));
                }
            }
        }
        out.extend(vec![0u8; 1024]); // end-of-archive marker
        out
    }

    pub struct LayerLayout {
        pub dir: PathBuf,
        pub manifest_digest: String,
    }

    impl Drop for LayerLayout {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Build an OCI layout whose layers are REAL tar byte strings.
    /// Index annotation ref = "fixture:latest".
    pub fn build_layout_with_layers(layers: Vec<Vec<u8>>) -> LayerLayout {
        let dir = tmpdir("laytar");
        let blobs = dir.join("blobs").join("sha256");
        fs::create_dir_all(&blobs).unwrap();

        let mut layer_descs = Vec::new();
        let mut diff_ids = Vec::new();
        for bytes in &layers {
            let d = sha256_hex(bytes);
            fs::write(blobs.join(&d), bytes).unwrap();
            diff_ids.push(format!("sha256:{d}"));
            layer_descs.push(json!({
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": format!("sha256:{d}"),
                "size": bytes.len(),
            }));
        }

        let config = json!({
            "architecture": std::env::consts::ARCH.replace("x86_64", "amd64")
                .replace("aarch64", "arm64"),
            "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": diff_ids},
            "config": {},
        });
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = sha256_hex(&config_bytes);
        fs::write(blobs.join(&config_digest), &config_bytes).unwrap();

        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": format!("sha256:{config_digest}"),
                "size": config_bytes.len(),
            },
            "layers": layer_descs,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = sha256_hex(&manifest_bytes);
        fs::write(blobs.join(&manifest_digest), &manifest_bytes).unwrap();

        let index = json!({
            "schemaVersion": 2,
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": format!("sha256:{manifest_digest}"),
                "size": manifest_bytes.len(),
                "annotations": {"org.opencontainers.image.ref.name": "fixture:latest"},
            }],
        });
        fs::write(dir.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
        fs::write(dir.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();

        LayerLayout {
            dir,
            manifest_digest,
        }
    }

    /// A live file/meta snapshot of a produced rootfs tree for assertions.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Node {
        pub kind: String, // "file" | "dir" | "symlink"
        pub mode: u32,
        pub data: Option<Vec<u8>>,
        pub link_target: Option<String>,
    }

    pub fn snap(root: &Path) -> Vec<(String, Node)> {
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Node)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            let md = fs::symlink_metadata(&p).unwrap();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                md.permissions().mode() & 0o7777
            };
            #[cfg(not(unix))]
            let mode = 0;
            if md.file_type().is_symlink() {
                out.push((
                    rel,
                    Node {
                        kind: "symlink".into(),
                        mode,
                        data: None,
                        link_target: Some(
                            fs::read_link(&p).unwrap().to_string_lossy().to_string(),
                        ),
                    },
                ));
                continue; // never follow, never traverse through links
            }
            if md.is_dir() {
                out.push((
                    rel,
                    Node {
                        kind: "dir".into(),
                        mode,
                        data: None,
                        link_target: None,
                    },
                ));
                walk(base, &p, out);
            } else {
                out.push((
                    rel,
                    Node {
                        kind: "file".into(),
                        mode,
                        data: Some(fs::read(&p).unwrap()),
                        link_target: None,
                    },
                ));
            }
        }
    }
}
