//! Content-addressed image store (T06-C01/C04).
//!
//! * import = stage (copy blobs + build refs dir) -> verify -> ONE rename
//!   publishes the only visible object (`refs/<manifest-hex>/`); there is no
//!   moment in which a half-written image is listable;
//! * verification failure moves the staged bundle under `quarantine/` with a
//!   machine-readable reason; the runnable list never sees the digest;
//! * concurrent import of the same immutable digest converges to exactly one
//!   complete visible object: rename is atomic on the same filesystem, the
//!   losing importer discards its own staging and returns the winner's ref.

use crate::images::error::{ImageError, Result};
use crate::images::layout::{blob_path, load_verified};
use crate::images::{ImageRef, Platform};
use ls_core::domain::ids::BundleRef;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub image: ImageRef,
    /// true when the object already existed (idempotent re-import).
    pub already_present: bool,
}

pub struct ImageStore {
    root: PathBuf,
}

fn store_dirs(root: &Path) -> Result<()> {
    for d in ["blobs/sha256", "refs", "staging", "quarantine"] {
        fs::create_dir_all(root.join(d))?;
    }
    Ok(())
}

impl ImageStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        store_dirs(root.as_ref())?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runnable list = directories under refs/. Quarantine is NOT a source.
    pub fn list_runnable(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("refs"))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.file_name().to_string_lossy().to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Import a verified OCI layout into the store.
    ///
    /// Digest failures (T06-C01) are recorded into quarantine with a
    /// machine-readable reason no matter WHERE in the pipeline they
    /// surface; platform/layout rejections are plain rejections (the
    /// artifact is unsupported or malformed, not evidence of corruption).
    pub fn import(
        &self,
        layout: &Path,
        reference: Option<&str>,
        host: &Platform,
        source_ref: &str,
        principal: &str,
    ) -> Result<ImportOutcome> {
        match self.import_inner(layout, reference, host, source_ref, principal) {
            Err(e @ ImageError::DigestMismatch { .. }) => {
                self.quarantine_record(&e, principal, source_ref);
                Err(e)
            }
            other => other,
        }
    }

    fn quarantine_record(&self, e: &ImageError, principal: &str, source_ref: &str) {
        if let ImageError::DigestMismatch {
            kind, expected, actual,
        } = e
        {
            let q = self.root.join("quarantine").join(format!(
                "{kind}-{}-{}",
                &expected[..16.min(expected.len())],
                nanos()
            ));
            let _ = fs::create_dir_all(&q);
            if let Ok(body) = serde_json::to_vec(&serde_json::json!({
                "code": e.code(),
                "blob_kind": kind,
                "expected_digest": expected,
                "actual_digest": actual,
                "detail": e.to_string(),
                "principal": principal,
                "source_ref": source_ref,
            })) {
                let _ = fs::write(q.join("reason.json"), body);
            }
        }
    }

    fn import_inner(
        &self,
        layout: &Path,
        reference: Option<&str>,
        host: &Platform,
        source_ref: &str,
        principal: &str,
    ) -> Result<ImportOutcome> {
        // 1. full verification FIRST (digests + platform, T06-C01/C03) —
        //    nothing enters the store before this passes.
        let loaded = load_verified(layout, reference, host)?;
        let hex = loaded
            .manifest_digest
            .strip_prefix("sha256:")
            .ok_or_else(|| ImageError::LayoutInvalid {
                detail: "manifest digest missing sha256: prefix".into(),
            })?
            .to_string();

        // 2. idempotent fast path: already published -> return the same ref.
        let refs_dir = self.root.join("refs").join(&hex);
        if refs_dir.is_dir() {
            return Ok(ImportOutcome {
                image: ImageRef {
                    digest: BundleRef::new(&loaded.manifest_digest).map_err(|e| {
                        ImageError::LayoutInvalid {
                            detail: format!("{e}"),
                        }
                    })?,
                    platform: loaded.platform.clone(),
                    config_digest: BundleRef::new(&loaded.config_digest).map_err(|e| {
                        ImageError::LayoutInvalid {
                            detail: format!("{e}"),
                        }
                    })?,
                },
                already_present: true,
            });
        }

        // 3. stage: blobs (tmp + rename per blob), then a complete refs dir
        //    assembled under staging/ and published with ONE rename.
        let staging = self
            .root
            .join("staging")
            .join(format!(
                "import-{}-{:?}-{}",
                std::process::id(),
                std::thread::current().id(),
                nanos()
            ));
        fs::create_dir_all(&staging)?;

        let outcome = (|| -> Result<ImportOutcome> {
            // 3a. blobs: manifest, config, layers — hard link when possible
            //     (same fs), byte copy otherwise; each lands via rename.
            let mut digests = vec![loaded.manifest_digest.clone(), loaded.config_digest.clone()];
            digests.extend(loaded.layers.iter().map(|l| l.digest.clone()));
            for digest in &digests {
                let blob_hex = digest.strip_prefix("sha256:").unwrap();
                let target = self.root.join("blobs").join("sha256").join(blob_hex);
                if target.exists() {
                    continue; // content-addressed: identical bytes already there
                }
                let src = blob_path(layout, digest)?;
                // Publish by ATOMIC hard_link, not tmp+rename: POSIX says a
                // rename whose old and new paths are hard links to the SAME
                // inode "does nothing and returns success", so a racing
                // tmp+rename leaks the tmp file (observed: 19 leftover
                // blob-* files in the C04 race). hard_link either publishes
                // or reports the race outcome.
                match fs::hard_link(&src, &target) {
                    Ok(()) => {}
                    Err(_e) if target.exists() => {
                        // racer published the identical content-addressed
                        // bytes first — that is a win, not an error
                    }
                    Err(_) => {
                        // cross-fs / no-hardlink fallback: byte-copy through
                        // a tmp INSIDE our own staging dir so staging
                        // cleanup covers any failure mode.
                        let tmp = staging.join(format!("blob-{blob_hex}"));
                        fs::copy(&src, &tmp)?;
                        match fs::rename(&tmp, &target) {
                            Ok(()) => {}
                            Err(e) if target.exists() => {
                                let _ = fs::remove_file(&tmp);
                                let _ = e;
                            }
                            Err(e) => return Err(ImageError::from(e)),
                        }
                        let _ = fs::remove_file(&tmp);
                    }
                }
            }

            // 3b. lease + refs assembly (staging dir, not yet visible).
            let ref_stage = staging.join("refs");
            fs::create_dir_all(&ref_stage)?;
            fs::write(ref_stage.join("manifest.json"), &loaded.manifest_bytes)?;
            fs::write(
                ref_stage.join("meta.json"),
                serde_json::to_vec(&serde_json::json!({
                    "manifest_digest": loaded.manifest_digest,
                    "config_digest": loaded.config_digest,
                    "platform": {
                        "os": loaded.platform.os,
                        "architecture": loaded.platform.architecture,
                        "variant": loaded.platform.variant,
                    },
                    "layers": loaded.layers.iter().map(|l| l.digest.clone())
                        .collect::<Vec<_>>(),
                }))?,
            )?;
            fs::write(
                ref_stage.join("lease.json"),
                serde_json::to_vec(&serde_json::json!({
                    "principal": principal,
                    "source_ref": source_ref,
                    "imported_at_unix_ms": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                }))?,
            )?;

            // 3c. atomic publish. Loser of a concurrent race unlinks its own
            //     staging and reports the winner's identical ref (T06-C04).
            match fs::rename(&ref_stage, &refs_dir) {
                Ok(()) => Ok(ImportOutcome {
                    image: ImageRef {
                        digest: BundleRef::new(&loaded.manifest_digest).map_err(|e| {
                            ImageError::LayoutInvalid {
                                detail: format!("{e}"),
                            }
                        })?,
                        platform: loaded.platform.clone(),
                        config_digest: BundleRef::new(&loaded.config_digest).map_err(|e| {
                            ImageError::LayoutInvalid {
                                detail: format!("{e}"),
                            }
                        })?,
                    },
                    already_present: false,
                }),
                Err(e) if refs_dir.is_dir() => Ok(ImportOutcome {
                    image: ImageRef {
                        digest: BundleRef::new(&loaded.manifest_digest).map_err(|e2| {
                            ImageError::LayoutInvalid {
                                detail: format!("{e2}"),
                            }
                        })?,
                        platform: loaded.platform.clone(),
                        config_digest: BundleRef::new(&loaded.config_digest).map_err(|e2| {
                            ImageError::LayoutInvalid {
                                detail: format!("{e2}"),
                            }
                        })?,
                    },
                    already_present: true,
                }),
                Err(e) => Err(ImageError::Io {
                    detail: format!("publish rename failed: {e}"),
                }),
            }
        })();

        // 4. either way clean the staging dir; digest-evidence recording is
        //    the public wrapper's job (covers load-time failures too).
        match outcome {
            Ok(o) => {
                let _ = fs::remove_dir_all(&staging);
                Ok(o)
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&staging);
                Err(e)
            }
        }
    }

    /// Verification-failure quarantine path for the digest (evidence sink).
    pub fn quarantine_dir(&self) -> PathBuf {
        self.root.join("quarantine")
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::testkit::{build_layout, corrupt_blob, tmpdir};
    use serde_json::json;

    fn host() -> Platform {
        Platform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: None,
        }
    }

    #[test]
    fn import_publishes_exact_imageref_and_lease() {
        let img = build_layout("amd64", json!(["/init"]), json!(["run"]), json!([]), "", "", "");
        let store_root = tmpdir("store");
        let store = ImageStore::open(&store_root).unwrap();
        let out = store
            .import(&img.dir, None, &host(), "fixture:latest", "tester")
            .unwrap();
        assert!(!out.already_present);
        assert_eq!(out.image.digest.as_str(), format!("sha256:{}", img.manifest_digest));
        assert_eq!(out.image.config_digest.as_str(), format!("sha256:{}", img.config_digest));
        assert_eq!(store.list_runnable().unwrap(), vec![img.manifest_digest.clone()]);
        let lease: serde_json::Value = serde_json::from_slice(
            &fs::read(
                store_root
                    .join("refs")
                    .join(&img.manifest_digest)
                    .join("lease.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(lease["principal"].as_str().unwrap(), "tester");
        // idempotent re-import: no duplicate object, already_present flips
        let again = store
            .import(&img.dir, None, &host(), "fixture:latest", "tester")
            .unwrap();
        assert!(again.already_present);
        assert_eq!(again.image, out.image);
        assert_eq!(store.list_runnable().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&store_root);
    }

    // ------------------------------------------------------------- T06-C01

    #[test]
    fn c01_corrupted_image_never_enters_runnable_list_only_quarantine() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        corrupt_blob(&img, &img.layer_digests[0]);
        let store_root = tmpdir("store-c01");
        let store = ImageStore::open(&store_root).unwrap();
        let err = store
            .import(&img.dir, None, &host(), "fixture:latest", "tester")
            .unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
        // NOT in the runnable list; sandbox creation has nothing to address
        assert!(store.list_runnable().unwrap().is_empty());
        // quarantine holds a machine-readable reason
        let q: Vec<_> = fs::read_dir(store.quarantine_dir())
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(q.len(), 1);
        let reason: serde_json::Value =
            serde_json::from_slice(&fs::read(q[0].path().join("reason.json")).unwrap()).unwrap();
        assert_eq!(reason["code"].as_str().unwrap(), "IMAGE_DIGEST_MISMATCH");
        // no staging residue
        assert_eq!(
            fs::read_dir(store_root.join("staging")).unwrap().count(),
            0
        );
        let _ = fs::remove_dir_all(&store_root);
    }

    #[test]
    fn c03_wrong_platform_import_also_stays_out_of_runnable() {
        let img = build_layout("riscv64", json!(null), json!(["ash"]), json!([]), "", "", "");
        let store_root = tmpdir("store-c03");
        let store = ImageStore::open(&store_root).unwrap();
        let err = store
            .import(&img.dir, None, &host(), "fixture:latest", "tester")
            .unwrap_err();
        assert_eq!(err.code(), "HOST_UNSUPPORTED");
        assert!(store.list_runnable().unwrap().is_empty());
        let _ = fs::remove_dir_all(&store_root);
    }

    // ------------------------------------------------------------- T06-C04

    #[test]
    fn c04_concurrent_same_digest_import_yields_one_complete_object() {
        let img = build_layout("amd64", json!(["/init"]), json!(["run"]), json!([]), "", "", "");
        let store_root = tmpdir("store-c04");
        let store = ImageStore::open(&store_root).unwrap();

        // pre-verify outside the race so threads start close together
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store_root = store_root.clone();
            let img_dir = img.dir.clone();
            let barrier = barrier.clone();
            let manifest_digest = img.manifest_digest.clone();
            handles.push(std::thread::spawn(move || {
                let store = ImageStore::open(&store_root).unwrap();
                barrier.wait();
                let out = store
                    .import(&img_dir, None, &host(), "fixture:latest", "tester")
                    .unwrap();
                assert_eq!(
                    out.image.digest.as_str(),
                    format!("sha256:{manifest_digest}")
                );
                out
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // exactly one visible ref, and exactly one importer created it
        assert_eq!(store.list_runnable().unwrap().len(), 1);
        assert_eq!(
            results.iter().filter(|r| !r.already_present).count(),
            1,
            "exactly one importer may publish the object"
        );
        // the visible object is complete: manifest + meta + lease present
        let ref_dir = store_root.join("refs").join(&img.manifest_digest);
        for f in ["manifest.json", "meta.json", "lease.json"] {
            assert!(ref_dir.join(f).is_file(), "missing {f}: half-published ref!");
        }
        let published = fs::read(ref_dir.join("manifest.json")).unwrap();
        assert_eq!(published, img.manifest_bytes);
        // no staging residue after the race
        assert_eq!(
            fs::read_dir(store_root.join("staging")).unwrap().count(),
            0,
            "staging residue left behind"
        );
        // and the published content is exactly the 3 blobs, nothing partial
        assert_eq!(
            fs::read_dir(store_root.join("blobs").join("sha256"))
                .unwrap()
                .count(),
            3,
            "blob store must hold exactly manifest+config+layer"
        );
        let _ = std::sync::Arc::into_inner(barrier);
        let _ = fs::remove_dir_all(&store_root);
    }
}
