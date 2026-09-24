//! Typed identities (T04). Server-generated opaque ids only; names and PIDs
//! are never security identity (docs/v5/03_DOMAIN.md §资源与生命周期).
//!
//! Validation is uniform: 1..=128 chars of `[A-Za-z0-9._-]`. Anything else —
//! empty, whitespace, `/`, NUL, non-ASCII — is rejected at the boundary with
//! INVALID_ARGUMENT naming the offending field kind.

use super::error::DomainError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const MAX_ID_LEN: usize = 128;

fn validate_id(kind: &str, raw: &str) -> Result<String, DomainError> {
    if raw.is_empty() {
        return Err(DomainError::invalid(format!("{kind} must not be empty")));
    }
    if raw.len() > MAX_ID_LEN {
        return Err(DomainError::invalid(format!(
            "{kind} exceeds {MAX_ID_LEN} chars"
        )));
    }
    for ch in raw.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_');
        if !ok {
            return Err(DomainError::invalid(format!(
                "{kind} contains illegal character {:?}",
                ch
            )));
        }
    }
    Ok(raw.to_string())
}

macro_rules! id_type {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: impl AsRef<str>) -> Result<Self, DomainError> {
                validate_id($kind, raw.as_ref()).map(Self)
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

id_type!(SandboxId, "sandbox_id");
id_type!(TemplateId, "template_id");
id_type!(OperationId, "operation_id");
id_type!(ExecutionId, "execution_id");
id_type!(CleanupId, "cleanup_id");
id_type!(LeaseId, "lease_id");
/// The authenticated principal. Never taken from a request body at the API
/// boundary (docs/v5/06_HTTP.md); established from the trusted identity.
id_type!(OwnerId, "owner_id");

/// Incarnation counter: fences a sandbox generation; an API/manager restart
/// reloads it unchanged. Distinct from [`Revision`] — never mixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Generation(pub u64);

impl Generation {
    pub const INITIAL: Generation = Generation(1);
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn checked_next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Generation)
            .ok_or_else(|| DomainError::invalid("generation overflow"))
    }
}

/// Record-state CAS counter within one incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Revision(pub u64);

impl Revision {
    pub const INITIAL: Revision = Revision(0);
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn checked_next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Revision)
            .ok_or_else(|| DomainError::invalid("revision overflow"))
    }
}

/// Content-addressed rootfs/image reference: `sha256:<64 lowercase hex>`.
/// The control protocol addresses bundles ONLY by digest — a host path can
/// never be smuggled where a bundle is expected (T04-C03).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleRef(String);

impl BundleRef {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, DomainError> {
        let raw = raw.as_ref();
        let Some(hex) = raw.strip_prefix("sha256:") else {
            return Err(DomainError::invalid(
                "bundle must be a sha256:<hex> digest reference",
            ));
        };
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(DomainError::invalid(
                "bundle digest must be 64 lowercase hex chars",
            ));
        }
        Ok(BundleRef(raw.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A path INSIDE the guest rootfs. Absolute, no `.`/`..` components, no empty
/// components, no NUL. Normalisation is the caller's job; the boundary rejects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GuestPath(String);

impl GuestPath {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, DomainError> {
        let raw = raw.as_ref();
        if raw.is_empty() {
            return Err(DomainError::invalid("guest path must not be empty"));
        }
        if raw.chars().any(|c| c == '\0') {
            return Err(DomainError::invalid("guest path contains NUL"));
        }
        if !raw.starts_with('/') {
            return Err(DomainError::invalid("guest path must be absolute"));
        }
        // "/" itself is the legal guest root; every other path is checked
        // component-wise (no empty, "." or ".." segments).
        if raw != "/" {
            for comp in raw.split('/').skip(1) {
                if comp.is_empty() || comp == "." || comp == ".." {
                    return Err(DomainError::invalid(format!(
                        "guest path has illegal component {comp:?}"
                    )));
                }
            }
        }
        Ok(GuestPath(raw.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids_roundtrip() {
        let id = SandboxId::new("sb_01.A-b").unwrap();
        assert_eq!(id.as_str(), "sb_01.A-b");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"sb_01.A-b\"");
        let back: SandboxId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn rejects_empty_overlong_and_illegal_chars() {
        assert!(SandboxId::new("").is_err());
        assert!(SandboxId::new("x".repeat(MAX_ID_LEN + 1)).is_err());
        assert!(SandboxId::new("has space").is_err());
        assert!(SandboxId::new("has/slash").is_err());
        assert!(SandboxId::new(format!("nul{}byte", '\0')).is_err());
        assert!(SandboxId::new("沙盒").is_err());
        let err = SandboxId::new("has/slash").unwrap_err();
        assert_eq!(err.code, super::super::error::ErrorCode::InvalidArgument);
        assert!(err.message.contains("sandbox_id"));
    }

    #[test]
    fn generation_and_revision_are_distinct_and_checked() {
        let g = Generation::INITIAL.checked_next().unwrap();
        assert_eq!(g.get(), 2);
        assert!(Generation(u64::MAX).checked_next().is_err());
        assert!(Revision(u64::MAX).checked_next().is_err());
    }

    #[test]
    fn bundle_ref_requires_digest_shape() {
        let ok = format!("sha256:{}", "a".repeat(64));
        assert!(BundleRef::new(&ok).is_ok());
        assert!(BundleRef::new("debian:latest").is_err());
        assert!(BundleRef::new(format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(BundleRef::new(format!("sha256:{}", "a".repeat(63))).is_err());
        assert!(BundleRef::new("/host/path/rootfs").is_err());
    }

    #[test]
    fn guest_path_rules() {
        assert!(GuestPath::new("/app").is_ok());
        assert!(GuestPath::new("/").is_ok());
        assert!(GuestPath::new("").is_err());
        assert!(GuestPath::new("app").is_err());
        assert!(GuestPath::new("/a/../b").is_err());
        assert!(GuestPath::new("/a/./b").is_err());
        assert!(GuestPath::new("/a//b").is_err());
        assert!(GuestPath::new("/a/").is_err());
        assert!(GuestPath::new(format!("/a/{}", '\0')).is_err());
    }
}
