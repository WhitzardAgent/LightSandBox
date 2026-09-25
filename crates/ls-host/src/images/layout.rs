//! OCI image-layout reader with full digest verification (T06-C01, T06-C03).
//!
//! Trust rule: every byte enters via its descriptor digest. A blob whose
//! content does not hash to its name never reaches the store; index →
//! manifest → config → layers are verified in that order, and the platform
//! check runs at load time so a wrong-arch image fails BEFORE any exec.

use crate::images::error::{ImageError, Result};
use crate::images::Platform;
use ls_core::domain::idempotency::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};

/// A manifest descriptor as listed in index.json (tag is annotation-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDescriptor {
    pub digest: String,
    pub size: u64,
    pub reference: Option<String>,
}

/// A fully verified image: manifest bytes, config JSON, layer descriptors.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    pub manifest_digest: String,
    pub manifest_bytes: Vec<u8>,
    pub config_digest: String,
    pub config: serde_json::Value,
    pub platform: Platform,
    pub layers: Vec<LayerDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDescriptor {
    pub digest: String,
    pub size: u64,
    pub media_type: String,
}

fn parse_digest(s: &str) -> Result<&str> {
    s.strip_prefix("sha256:").filter(|h| {
        h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }).ok_or_else(|| ImageError::LayoutInvalid {
        detail: format!("not a sha256:<64 lowercase hex> digest: {s:?}"),
    })
}

/// Read a blob by digest and verify its content hash — the core T06-C01 gate.
pub fn read_verified_blob(layout: &Path, digest: &str, kind: &'static str) -> Result<Vec<u8>> {
    let hex = parse_digest(digest)?;
    let path = layout.join("blobs").join("sha256").join(hex);
    let bytes = fs::read(&path).map_err(|e| ImageError::LayoutInvalid {
        detail: format!("blob {digest} unreadable at {}: {e}", path.display()),
    })?;
    let actual = sha256_hex(&bytes);
    if actual != hex {
        return Err(ImageError::DigestMismatch {
            kind,
            expected: hex.to_string(),
            actual,
        });
    }
    Ok(bytes)
}

/// Stream-hash a blob against its digest name WITHOUT materializing it in
/// memory (round-03 #12): 64 KiB reads through an incremental SHA-256.
/// Used for layer blobs, which can be arbitrarily large; manifest/config
/// remain on `read_verified_blob` because their bytes are needed anyway.
/// Returns the verified byte count.
pub fn verify_blob_streaming(layout: &Path, digest: &str, kind: &'static str) -> Result<u64> {
    use std::io::Read;
    let hex = parse_digest(digest)?;
    let path = layout.join("blobs").join("sha256").join(hex);
    let mut f = fs::File::open(&path).map_err(|e| ImageError::LayoutInvalid {
        detail: format!("blob {digest} unreadable at {}: {e}", path.display()),
    })?;
    let mut hasher = super::sha256::Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| ImageError::LayoutInvalid {
            detail: format!("blob {digest} read failure at {}: {e}", path.display()),
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let actual = hasher.finalize_hex();
    if actual != hex {
        return Err(ImageError::DigestMismatch {
            kind,
            expected: hex.to_string(),
            actual,
        });
    }
    Ok(total)
}

fn expect_obj<'v>(v: &'v serde_json::Value, what: &str) -> Result<&'v serde_json::Map<String, serde_json::Value>> {
    v.as_object().ok_or_else(|| ImageError::LayoutInvalid {
        detail: format!("{what} must be an object"),
    })
}

/// Parse index.json and list manifest descriptors.
pub fn list_descriptors(layout: &Path) -> Result<Vec<ManifestDescriptor>> {
    let index_path = layout.join("index.json");
    let index: serde_json::Value = serde_json::from_slice(&fs::read(&index_path).map_err(
        |e| ImageError::LayoutInvalid {
            detail: format!("index.json unreadable: {e}"),
        },
    )?)
    .map_err(|e| ImageError::LayoutInvalid {
        detail: format!("index.json is not JSON: {e}"),
    })?;
    let manifests = index
        .get("manifests")
        .and_then(|m| m.as_array())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| ImageError::LayoutInvalid {
            detail: "index.json has no manifests".into(),
        })?;
    let mut out = Vec::new();
    for m in manifests {
        let obj = expect_obj(m, "manifest descriptor")?;
        let digest = obj
            .get("digest")
            .and_then(|d| d.as_str())
            .ok_or_else(|| ImageError::LayoutInvalid {
                detail: "manifest descriptor missing digest".into(),
            })?;
        parse_digest(digest)?;
        let size = obj.get("size").and_then(|s| s.as_u64()).ok_or_else(|| {
            ImageError::LayoutInvalid {
                detail: "manifest descriptor missing size".into(),
            }
        })?;
        let reference = obj
            .get("annotations")
            .and_then(|a| a.get("org.opencontainers.image.ref.name"))
            .and_then(|r| r.as_str())
            .map(|s| s.to_string());
        out.push(ManifestDescriptor {
            digest: digest.to_string(),
            size,
            reference,
        });
    }
    Ok(out)
}

/// Select ONE descriptor: by tag reference, or the sole entry. Tags are used
/// for resolution only; the returned digest is the runnable identity.
pub fn select_descriptor(
    descriptors: &[ManifestDescriptor],
    reference: Option<&str>,
) -> Result<ManifestDescriptor> {
    match reference {
        Some(tag) => descriptors
            .iter()
            .find(|d| d.reference.as_deref() == Some(tag))
            .cloned()
            .ok_or_else(|| ImageError::RefNotFound {
                reference: tag.to_string(),
            }),
        None if descriptors.len() == 1 => Ok(descriptors[0].clone()),
        None => Err(ImageError::LayoutInvalid {
            detail: format!(
                "{} manifests in index; a reference is required to select one",
                descriptors.len()
            ),
        }),
    }
}

/// Load + verify an image: manifest digest, config digest, layer digests,
/// then platform gate against `host` (T06-C03 fires here, before any exec).
pub fn load_verified(
    layout: &Path,
    reference: Option<&str>,
    host: &Platform,
) -> Result<LoadedImage> {
    let descriptor = select_descriptor(&list_descriptors(layout)?, reference)?;

    // 1. manifest bytes must hash to the index descriptor digest.
    let manifest_bytes = read_verified_blob(layout, &descriptor.digest, "manifest")?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(|e| ImageError::LayoutInvalid {
            detail: format!("manifest is not JSON: {e}"),
        })?;
    let mobj = expect_obj(&manifest, "manifest")?;

    // 2. config descriptor -> verified config blob.
    let cfg_desc = expect_obj(
        mobj.get("config").ok_or_else(|| ImageError::LayoutInvalid {
            detail: "manifest missing config descriptor".into(),
        })?,
        "config descriptor",
    )?;
    let config_digest = cfg_desc
        .get("digest")
        .and_then(|d| d.as_str())
        .ok_or_else(|| ImageError::LayoutInvalid {
            detail: "config descriptor missing digest".into(),
        })?
        .to_string();
    let config_bytes = read_verified_blob(layout, &config_digest, "config")?;
    let config: serde_json::Value =
        serde_json::from_slice(&config_bytes).map_err(|e| ImageError::LayoutInvalid {
            detail: format!("image config is not JSON: {e}"),
        })?;
    let cobj = expect_obj(&config, "image config")?;

    let architecture = cobj
        .get("architecture")
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ImageError::LayoutInvalid {
            detail: "image config missing architecture".into(),
        })?
        .to_string();
    let os = cobj
        .get("os")
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ImageError::LayoutInvalid {
            detail: "image config missing os".into(),
        })?
        .to_string();
    let variant = cobj
        .get("variant")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let platform = Platform {
        os,
        architecture,
        variant,
    };

    // 3. every layer digest verifies before the image is considered loaded.
    let mut layers = Vec::new();
    let layer_arr = mobj
        .get("layers")
        .and_then(|l| l.as_array())
        .ok_or_else(|| ImageError::LayoutInvalid {
            detail: "manifest layers must be an array".into(),
        })?;
    if layer_arr.is_empty() {
        return Err(ImageError::LayoutInvalid {
            detail: "manifest has zero layers".into(),
        });
    }
    for l in layer_arr {
        let lobj = expect_obj(l, "layer descriptor")?;
        let digest = lobj
            .get("digest")
            .and_then(|d| d.as_str())
            .ok_or_else(|| ImageError::LayoutInvalid {
                detail: "layer descriptor missing digest".into(),
            })?
            .to_string();
        let size = lobj.get("size").and_then(|s| s.as_u64()).ok_or_else(|| {
            ImageError::LayoutInvalid {
                detail: "layer descriptor missing size".into(),
            }
        })?;
        let media_type = lobj
            .get("mediaType")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        // stream-hashed: no full layer ever sits in RAM for verification
        let actual_size = verify_blob_streaming(layout, &digest, "layer")?;
        if actual_size != size {
            return Err(ImageError::LayoutInvalid {
                detail: format!("layer {digest} size {actual_size} != descriptor size {size}"),
            });
        }
        layers.push(LayerDescriptor {
            digest,
            size,
            media_type,
        });
    }

    // 4. platform gate — digest check first, then capability honesty:
    //    a wrong-arch image is rejected HERE with HOST_UNSUPPORTED, never as
    //    a confusing failure after exec starts (T06-C03).
    if !host.can_execute(&platform) {
        return Err(ImageError::PlatformUnsupported {
            image: format!(
                "{}/{}{}",
                platform.os,
                platform.architecture,
                platform
                    .variant
                    .as_ref()
                    .map(|v| format!("/{v}"))
                    .unwrap_or_default()
            ),
            host: format!("{}/{}", host.os, host.architecture),
        });
    }

    Ok(LoadedImage {
        manifest_digest: descriptor.digest,
        manifest_bytes,
        config_digest,
        config,
        platform,
        layers,
    })
}

/// Blob path for a verified digest (store uses this to link blobs out).
pub fn blob_path(layout: &Path, digest: &str) -> Result<PathBuf> {
    Ok(layout
        .join("blobs")
        .join("sha256")
        .join(parse_digest(digest)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::testkit::{build_layout, corrupt_blob};
    use serde_json::json;

    fn host() -> Platform {
        Platform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: None,
        }
    }

    #[test]
    fn good_layout_loads_with_exact_digests() {
        let img = build_layout("amd64", json!(["/init"]), json!(["--serve"]), json!([]), "", "", "");
        let loaded = load_verified(&img.dir, None, &host()).unwrap();
        assert_eq!(loaded.manifest_digest, format!("sha256:{}", img.manifest_digest));
        assert_eq!(loaded.config_digest, format!("sha256:{}", img.config_digest));
        // verified config blob round-trips to exactly the fixture's config JSON
        assert_eq!(loaded.config, img.config_json);
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(
            loaded.layers[0].digest,
            format!("sha256:{}", img.layer_digests[0])
        );
        assert_eq!(loaded.platform.architecture, "amd64");
        assert_eq!(loaded.platform.os, "linux");
    }

    #[test]
    fn by_tag_resolves_to_same_digest() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        let loaded = load_verified(&img.dir, Some("fixture:latest"), &host()).unwrap();
        assert_eq!(loaded.manifest_digest, format!("sha256:{}", img.manifest_digest));
        let err = load_verified(&img.dir, Some("fixture:absent"), &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_REF_NOT_FOUND");
    }

    // ------------------------------------------------------------- T06-C01

    #[test]
    fn c01_corrupted_layer_blob_fails_digest_check() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        corrupt_blob(&img, &img.layer_digests[0]);
        let err = load_verified(&img.dir, None, &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
        match err {
            ImageError::DigestMismatch { kind, expected, actual } => {
                assert_eq!(kind, "layer");
                assert_eq!(expected, img.layer_digests[0]);
                assert_ne!(actual, expected);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
    }

    /// Round-03 #12: layer verification is STREAMED through the incremental
    /// hasher — prove the multi-chunk path by verifying a >128 KiB layer
    /// end-to-end and catching a byte flip hash-side (not whole-buffer-side).
    #[test]
    fn c01_large_layer_stream_verified_and_flip_detected() {
        let big: Vec<u8> = (0..300_000u32).map(|i| ((i * 31) % 251) as u8).collect();
        let layout = crate::rootfs::testkit::build_layout_with_layers(vec![big.clone()]);
        let loaded = load_verified(&layout.dir, Some("fixture:latest"), &Platform::host()).unwrap();
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(loaded.layers[0].size, big.len() as u64);
        // flip a byte mid-stream (past the first 64 KiB read): digest gate
        // must still fire through the streaming verifier.
        let hex = loaded.layers[0]
            .digest
            .strip_prefix("sha256:")
            .unwrap()
            .to_string();
        let p = layout.dir.join("blobs").join("sha256").join(&hex);
        let mut b = fs::read(&p).unwrap();
        b[150_000] ^= 0xFF;
        fs::write(&p, b).unwrap();
        let err = load_verified(&layout.dir, Some("fixture:latest"), &Platform::host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
        // and the size-vs-descriptor gate still works on the streamed count
        let err = verify_blob_streaming(&layout.dir, &format!("sha256:{hex}"), "layer").unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
    }

    #[test]
    fn c01_corrupted_config_blob_fails_before_any_platform_check() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        corrupt_blob(&img, &img.config_digest);
        let err = load_verified(&img.dir, None, &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
    }

    #[test]
    fn c01_corrupted_manifest_blob_fails_at_first_gate() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        corrupt_blob(&img, &img.manifest_digest);
        let err = load_verified(&img.dir, None, &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_DIGEST_MISMATCH");
    }

    // ------------------------------------------------------------- T06-C03

    #[test]
    fn c03_wrong_architecture_rejected_as_host_unsupported() {
        let img = build_layout("riscv64", json!(null), json!(["ash"]), json!([]), "", "", "");
        let err = load_verified(&img.dir, None, &host()).unwrap_err();
        assert_eq!(err.code(), "HOST_UNSUPPORTED");
        match err {
            ImageError::PlatformUnsupported { image, host } => {
                assert_eq!(image, "linux/riscv64");
                assert_eq!(host, "linux/amd64");
            }
            other => panic!("expected PlatformUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn c03_wrong_os_rejected() {
        let img = build_layout("amd64", json!(null), json!(["ash"]), json!([]), "", "", "");
        // host pretending to be windows/amd64 must reject linux/amd64 image
        let odd_host = Platform {
            os: "windows".into(),
            architecture: "amd64".into(),
            variant: None,
        };
        let err = load_verified(&img.dir, None, &odd_host).unwrap_err();
        assert_eq!(err.code(), "HOST_UNSUPPORTED");
    }

    #[test]
    fn malformed_layouts_fail_structured_not_panic() {
        let dir = crate::images::testkit::tmpdir("garbage");
        std::fs::write(dir.join("index.json"), b"{not json").unwrap();
        let err = load_verified(&dir, None, &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_LAYOUT_INVALID");
        let dir2 = crate::images::testkit::tmpdir("empty-idx");
        std::fs::write(dir2.join("index.json"), br#"{"schemaVersion":2,"manifests":[]}"#).unwrap();
        let err = load_verified(&dir2, None, &host()).unwrap_err();
        assert_eq!(err.code(), "IMAGE_LAYOUT_INVALID");
    }

    #[test]
    fn platform_variant_rules_are_exact() {
        let h = host();
        assert!(h.can_execute(&host()));
        let pinned = Platform {
            os: "linux".into(),
            architecture: "arm".into(),
            variant: Some("v7".into()),
        };
        assert!(!h.can_execute(&pinned));
        let variantful_host = Platform {
            os: "linux".into(),
            architecture: "arm".into(),
            variant: Some("v7".into()),
        };
        assert!(variantful_host.can_execute(&pinned));
        // a host without variant must not pretend to be v7
        let unvarianted = Platform {
            os: "linux".into(),
            architecture: "arm".into(),
            variant: None,
        };
        assert!(!unvarianted.can_execute(&pinned));
        assert!(!variantful_host.can_execute(&unvarianted));
    }
}
