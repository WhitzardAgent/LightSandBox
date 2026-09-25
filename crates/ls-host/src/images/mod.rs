//! T06 — OCI image import, digest verification, platform gating and
//! deterministic image-config → process-spec conversion.
//!
//! Boundaries (plans/tasks/T06.md):
//! * tags are for resolution only; everything runnable is addressed by
//!   `sha256:<hex>` digest ([`ImageRef`]);
//! * import is atomic: staging → full digest verification → one rename
//!   publishes the only visible object; failed digests go to quarantine,
//!   never to the runnable list;
//! * this module NEVER runs image content, never interpolates a shell, and
//!   never lets registry credentials reach argv, guest env or logs;
//! * empty-string vs unset field semantics are preserved exactly
//!   (review focus: they are different things).

pub mod error;
pub mod layout;
pub mod process_config;
pub mod pull;
pub mod store;

pub use error::{ImageError, Result};
pub use layout::{LoadedImage, ManifestDescriptor};
pub use process_config::{ImageProcessConfig, ProcessOverrides, ProcessSpec};
pub use pull::{FileTransport, PullOutcome, PullRequest, RegistryCreds, SkopeoTransport, Transport};
pub use store::{ImageStore, ImportOutcome};

use ls_core::domain::ids::BundleRef;

/// OCI platform tuple. Exactly one deterministic spelling is kept:
/// `os`/`architecture` follow the OCI/Go spelling (`linux`, `amd64`, `arm64`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

impl Platform {
    /// The platform of THIS host in OCI spelling. Explicit mapping table —
    /// no string munging that could drift per toolchain.
    pub fn host() -> Platform {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            "arm" => "arm",
            "riscv64" => "riscv64",
            other => other,
        };
        Platform {
            os: std::env::consts::OS.to_string(),
            architecture: architecture.to_string(),
            variant: None,
        }
    }

    /// Host can execute `img` when os/arch match exactly. A host variant of
    /// None never matches an image that pins one (deterministic, no fuzzy
    /// "close enough" matching — mismatch must fail BEFORE execution).
    pub fn can_execute(&self, img: &Platform) -> bool {
        self.os == img.os
            && self.architecture == img.architecture
            && match (&self.variant, &img.variant) {
                (None, None) => true,
                (Some(h), Some(i)) => h == i,
                (None, Some(_)) => false,
                (Some(_), None) => false,
            }
    }
}

/// The only runnable image handle: manifest digest + verified platform +
/// config digest. Tags never appear here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// sha256 digest of the manifest (the index descriptor digest).
    pub digest: BundleRef,
    pub platform: Platform,
    /// sha256 digest of the image config blob.
    pub config_digest: BundleRef,
}

#[cfg(test)]
pub(crate) mod testkit {
    //! Deterministic OCI image-layout fixture builder. Layers are opaque
    //! byte blobs: T06 verifies digests and configs; unpacking layer content
    //! belongs to the rootfs task graph entry after this one.
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    use ls_core::domain::idempotency::sha256_hex;

    pub struct FixtureImage {
        pub dir: PathBuf,
        pub manifest_digest: String,
        pub config_digest: String,
        pub layer_digests: Vec<String>,
        pub manifest_bytes: Vec<u8>,
        pub config_json: Value,
    }

    impl Drop for FixtureImage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    pub fn tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("ls-t06-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Build a syntactically valid OCI image layout. `entrypoint`/`cmd` are
    /// passed as raw JSON values so tests can express null vs [] vs [""].
    #[allow(clippy::too_many_arguments)]
    pub fn build_layout(
        arch: &str,
        entrypoint: Value,
        cmd: Value,
        env: Value,
        working_dir: &str,
        user: &str,
        stop_signal: &str,
    ) -> FixtureImage {
        let dir = tmpdir("layout");
        let blobs = dir.join("blobs").join("sha256");
        fs::create_dir_all(&blobs).unwrap();

        // ---- layer blob(s): opaque payload, digest-addressed
        let layer_bytes: Vec<u8> = b"light-sandbox fixture layer payload v1\n".to_vec();
        let layer_digest = sha256_hex(&layer_bytes);
        fs::write(blobs.join(&layer_digest), &layer_bytes).unwrap();

        // ---- config blob
        let mut config = json!({
            "architecture": arch,
            "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": [format!("sha256:{layer_digest}")]},
            "config": {},
        });
        // Empty-string vs unset: emit exactly the keys the case asked for
        // (raw JSON value in -> same presence out).
        if !entrypoint.is_null() {
            config["config"]["Entrypoint"] = entrypoint;
        }
        if !cmd.is_null() {
            config["config"]["Cmd"] = cmd;
        }
        if !env.is_null() {
            config["config"]["Env"] = env;
        }
        if !working_dir.is_empty() {
            config["config"]["WorkingDir"] = json!(working_dir);
        }
        if !user.is_empty() {
            config["config"]["User"] = json!(user);
        }
        if !stop_signal.is_empty() {
            config["config"]["StopSignal"] = json!(stop_signal);
        }
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = sha256_hex(&config_bytes);
        fs::write(blobs.join(&config_digest), &config_bytes).unwrap();

        // ---- manifest blob
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": format!("sha256:{config_digest}"),
                "size": config_bytes.len(),
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": format!("sha256:{layer_digest}"),
                "size": layer_bytes.len(),
            }],
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = sha256_hex(&manifest_bytes);
        fs::write(blobs.join(&manifest_digest), &manifest_bytes).unwrap();

        // ---- index + oci-layout marker
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

        FixtureImage {
            dir,
            manifest_digest,
            config_digest,
            layer_digests: vec![layer_digest],
            manifest_bytes,
            config_json: config,
        }
    }

    /// Flip one byte inside the named blob — returns the corrupted path.
    pub fn corrupt_blob(img: &FixtureImage, digest: &str) -> PathBuf {
        let p = img.dir.join("blobs").join("sha256").join(digest);
        let mut bytes = fs::read(&p).unwrap();
        bytes[0] ^= 0xFF;
        fs::write(&p, &bytes).unwrap();
        p
    }
}
