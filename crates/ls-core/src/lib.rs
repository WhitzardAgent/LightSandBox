//! LightSandbox core domain vocabulary.
//!
//! T00 scope: host capability tiers and per-capability findings used by the
//! T03 HostReport. No HTTP, SQL, privileged syscalls or evaluation semantics
//! belong in this crate (see docs/v5/02_ARCHITECTURE.md and docs/v5/03_DOMAIN.md).

pub mod capability {
    use serde::{Deserialize, Serialize};
    use std::fmt;

    /// Admission tier of a host, derived only from measured findings.
    /// See docs/v5/03_DOMAIN.md §宿主能力分层.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum HostCapabilityTier {
        /// Single-ID userns; no subuid/uidmap; no cgroup delegation.
        PortableRootless,
        /// Multi-ID mapping, usable private proc, rootless networking,
        /// writable fs strategy, cgroup delegation when hard limits requested.
        FullRootless,
        /// Future optional backend, not the primary deployment target.
        Rootful,
    }

    /// Per-capability verdict. Every value binds to real evidence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum CapabilityStatus {
        Supported,
        Degraded,
        Unsupported,
        NotTested,
    }

    // Well-known finding names referenced by tier derivation.
    pub const USERNS_SINGLE_ID: &str = "userns.single_id_mapping";
    pub const USERNS_MULTI_ID: &str = "userns.multi_id_mapping";
    pub const CGROUP_DELEGATION: &str = "cgroup.delegation";
    pub const PROCFS_FRESH_MOUNT: &str = "procfs.fresh_mount";
    pub const ROOTFUL_CAPS: &str = "identity.rootful_cap_sys_admin";

    /// One measured capability statement with bound evidence.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CapabilityFinding {
        pub name: String,
        pub status: CapabilityStatus,
        /// Exact argv or read path used as evidence; never a shell string.
        pub evidence_command: String,
        #[serde(default)]
        pub exit_code: Option<i32>,
        #[serde(default)]
        pub output_excerpt: String,
    }

    impl CapabilityFinding {
        pub fn new(
            name: &str,
            status: CapabilityStatus,
            evidence_command: &str,
            exit_code: Option<i32>,
            output_excerpt: &str,
        ) -> Self {
            Self {
                name: name.to_string(),
                status,
                evidence_command: evidence_command.to_string(),
                exit_code,
                output_excerpt: output_excerpt.to_string(),
            }
        }
    }

    /// Machine-readable host capability report (T03 HostReport shape).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct HostCapabilityReport {
        pub probe_date: String,
        pub hostname: String,
        pub findings: Vec<CapabilityFinding>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TierEvidenceError(pub Vec<String>);

    impl fmt::Display for TierEvidenceError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "insufficient or negative evidence for capabilities: {}", self.0.join(", "))
        }
    }

    impl std::error::Error for TierEvidenceError {}

    impl HostCapabilityReport {
        fn status_of(&self, name: &str) -> Option<CapabilityStatus> {
            self.findings
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.status)
        }

        /// Derive the host tier strictly from measured findings.
        ///
        /// Rules (deterministic, conservative):
        /// * measured rootful capabilities => `Rootful`
        /// * multi-ID mapping + cgroup delegation + fresh proc all Supported => `FullRootless`
        /// * single-ID userns Supported => `PortableRootless`
        /// * anything untested or unsupported => `TierEvidenceError`
        pub fn derive_tier(&self) -> Result<HostCapabilityTier, TierEvidenceError> {
            if self.status_of(ROOTFUL_CAPS) == Some(CapabilityStatus::Supported) {
                return Ok(HostCapabilityTier::Rootful);
            }
            let full = [USERNS_MULTI_ID, CGROUP_DELEGATION, PROCFS_FRESH_MOUNT]
                .iter()
                .all(|n| self.status_of(n) == Some(CapabilityStatus::Supported));
            if full {
                return Ok(HostCapabilityTier::FullRootless);
            }
            let mut missing: Vec<String> = Vec::new();
            match self.status_of(USERNS_SINGLE_ID) {
                Some(CapabilityStatus::Supported) => return Ok(HostCapabilityTier::PortableRootless),
                other => {
                    missing.push(format!("{USERNS_SINGLE_ID} (observed {other:?})"));
                }
            }
            for name in [USERNS_MULTI_ID, CGROUP_DELEGATION, PROCFS_FRESH_MOUNT] {
                match self.status_of(name) {
                    Some(CapabilityStatus::NotTested) | None => missing.push(format!("{name} (not tested)")),
                    _ => {}
                }
            }
            Err(TierEvidenceError(missing))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn finding(name: &str, status: CapabilityStatus) -> CapabilityFinding {
            CapabilityFinding::new(name, status, "probe fixture", Some(0), "")
        }

        fn report(items: &[(&str, CapabilityStatus)]) -> HostCapabilityReport {
            HostCapabilityReport {
                probe_date: "2026-09-24".into(),
                hostname: "fixture".into(),
                findings: items.iter().map(|(n, s)| finding(n, *s)).collect(),
            }
        }

        #[test]
        fn single_id_only_derives_portable_rootless() {
            let r = report(&[
                (USERNS_SINGLE_ID, CapabilityStatus::Supported),
                (USERNS_MULTI_ID, CapabilityStatus::Unsupported),
                (CGROUP_DELEGATION, CapabilityStatus::Unsupported),
                (PROCFS_FRESH_MOUNT, CapabilityStatus::Unsupported),
            ]);
            assert_eq!(r.derive_tier().unwrap(), HostCapabilityTier::PortableRootless);
        }

        #[test]
        fn full_rootless_requires_all_three() {
            let r = report(&[
                (USERNS_SINGLE_ID, CapabilityStatus::Supported),
                (USERNS_MULTI_ID, CapabilityStatus::Supported),
                (CGROUP_DELEGATION, CapabilityStatus::Supported),
                (PROCFS_FRESH_MOUNT, CapabilityStatus::Supported),
            ]);
            assert_eq!(r.derive_tier().unwrap(), HostCapabilityTier::FullRootless);

            let r2 = report(&[
                (USERNS_SINGLE_ID, CapabilityStatus::Supported),
                (USERNS_MULTI_ID, CapabilityStatus::Supported),
                (CGROUP_DELEGATION, CapabilityStatus::Unsupported),
                (PROCFS_FRESH_MOUNT, CapabilityStatus::Supported),
            ]);
            assert_eq!(r2.derive_tier().unwrap(), HostCapabilityTier::PortableRootless);
        }

        #[test]
        fn rootful_marker_wins() {
            let r = report(&[(ROOTFUL_CAPS, CapabilityStatus::Supported)]);
            assert_eq!(r.derive_tier().unwrap(), HostCapabilityTier::Rootful);
        }

        #[test]
        fn untested_or_negative_evidence_is_error_not_assumption() {
            let r = report(&[(USERNS_SINGLE_ID, CapabilityStatus::Unsupported)]);
            let err = r.derive_tier().unwrap_err();
            assert!(err.0[0].contains(USERNS_SINGLE_ID));

            let empty = report(&[]);
            assert!(empty.derive_tier().is_err());
        }

        #[test]
        fn serde_names_are_screaming_snake() {
            let json = serde_json::to_string(&HostCapabilityTier::PortableRootless).unwrap();
            assert_eq!(json, "\"PORTABLE_ROOTLESS\"");
            let status = serde_json::to_string(&CapabilityStatus::NotTested).unwrap();
            assert_eq!(status, "\"NOT_TESTED\"");
        }

        #[test]
        fn report_roundtrips_through_json() {
            let r = report(&[(USERNS_SINGLE_ID, CapabilityStatus::Supported)]);
            let text = serde_json::to_string_pretty(&r).unwrap();
            let back: HostCapabilityReport = serde_json::from_str(&text).unwrap();
            assert_eq!(r, back);
        }
    }
}
