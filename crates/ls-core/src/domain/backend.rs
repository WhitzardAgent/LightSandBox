//! Backend-neutral sandbox runtime selection (ADR 0002, round 3).
//!
//! LightSandbox is a capability-adaptive multi-backend runtime. `runc` is the
//! implementation of the [`BackendKind::Native`] backend ONLY; it is not the
//! product. Users declare *what they need* ([`CapabilityRequirements`]); the
//! planner selects the lightest backend that satisfies every hard requirement
//! from measured per-backend facts ([`BackendCapabilities`]).
//!
//! Boundaries kept deliberately:
//! * this module is pure logic — no probing, no I/O; facts are supplied by
//!   hosts probes / host reports (T03-style) and spike results;
//! * binary names (`runc`, `linux (ARCH=um)`, `qemu-system-*`) are
//!   diagnostics ([`BackendCapabilities::implementation`]), never selection
//!   keys, and never part of the public request vocabulary;
//! * requirement-vs-observation separation is unchanged from T41: an
//!   unsatisfiable hard requirement REJECTS pre-execution, it never silently
//!   degrades (docs/v5/08_FAILURES.md; contracts/v1/resolution-rules.json).

use crate::contracts::{HostCapabilities, Status, UidMappingMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Runtime backend families. Wire names are implementation-free on purpose:
/// `Native` = OCI-style host-namespaced container (impl: runc);
/// `Kernel` = guest Linux kernel as a user process (impl: ARCH=um linux);
/// `Machine` = software-emulated machine (impl: qemu tcg);
/// `Sentry` = userspace-kernel sandbox (impl: gVisor) — deferred (ADR 0002 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackendKind {
    Native,
    Kernel,
    Machine,
    Sentry,
}

impl BackendKind {
    /// AUTO search order: lightest first. Sentry never auto-selected in v1
    /// (it does not solve the round-3 problems: strict-rootless multi-UID and
    /// full-system compatibility; see ADR 0002).
    pub const AUTO_ORDER: [BackendKind; 3] = [
        BackendKind::Native,
        BackendKind::Kernel,
        BackendKind::Machine,
    ];

    pub fn wire(&self) -> &'static str {
        match self {
            BackendKind::Native => "NATIVE",
            BackendKind::Kernel => "KERNEL",
            BackendKind::Machine => "MACHINE",
            BackendKind::Sentry => "SENTRY",
        }
    }
}

/// Public request vocabulary (ADR 0002 §6): users may express a preference,
/// never a binary. `auto` = planner chooses lightest satisfying backend.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BackendPreference {
    #[default]
    Auto,
    Native,
    Kernel,
    Machine,
}

/// Observed host deployment shape. This is a SEPARATE dimension from
/// `HostCapabilityTier` (PORTABLE_ROOTLESS / FULL_ROOTLESS / ROOTFUL): a plain
/// Linux VM and an outer Docker/OCI container can both be PORTABLE_ROOTLESS,
/// but they differ measurably (outer seccomp, masked /proc, cgroup namespace).
/// Round-3 dev worker facts: HostKind=CONTAINERIZED + PORTABLE_ROOTLESS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HostKind {
    Vm,
    Containerized,
    BareMetal,
}

/// Requirement dimension registry. These names match the T41 frozen
/// resolution-rules vocabulary where it already exists (`multi_uid`,
/// `hard_memory_limit`, `private_proc`, `seccomp`, `outbound_network`) and
/// extend it with the round-3 additions (`full_init_system`, `net_admin`).
/// The contract-surface evolution owning these names is task T45.
pub mod dim {
    pub const MULTI_UID: &str = "multi_uid";
    pub const PRIVATE_PROC: &str = "private_proc";
    pub const FULL_INIT_SYSTEM: &str = "full_init_system";
    pub const HARD_MEMORY_LIMIT: &str = "hard_memory_limit";
    pub const SECCOMP: &str = "seccomp";
    pub const OUTBOUND_NETWORK: &str = "outbound_network";
    pub const NET_ADMIN: &str = "net_admin";
    pub const ALL: [&str; 7] = [
        MULTI_UID,
        PRIVATE_PROC,
        FULL_INIT_SYSTEM,
        HARD_MEMORY_LIMIT,
        SECCOMP,
        OUTBOUND_NETWORK,
        NET_ADMIN,
    ];
}

/// Hard requirements declared by a caller (EnvironmentSpec side). Every `true`
/// bit MUST be observed `SUPPORTED` on the selected backend, else the plan is
/// rejected — never silently flattened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilityRequirements {
    /// Multiple guest-visible UIDs/GIDs (image USER != 0, chown/su semantics).
    #[serde(default)]
    pub multi_uid: bool,
    /// Guest-private /proc: no host process metadata/argv exposure.
    #[serde(default)]
    pub private_proc: bool,
    /// Full init system semantics (systemd-class: services, journal, cgroups
    /// as seen by the guest). Native backend is always UNSUPPORTED here.
    #[serde(default)]
    pub full_init_system: bool,
    /// Kernel-enforced memory ceiling (hard cgroup or equivalent).
    #[serde(default)]
    pub hard_memory_limit: bool,
    /// Syscall filtering available for the sandbox payload.
    #[serde(default)]
    pub seccomp: bool,
    /// Outbound TCP/DNS from inside the sandbox.
    #[serde(default)]
    pub outbound_network: bool,
    /// NET_ADMIN-class operations (iptables, interface control) inside the
    /// sandbox's own network domain.
    #[serde(default)]
    pub net_admin: bool,
}

impl CapabilityRequirements {
    /// Stable-ordered list of required-true dimensions.
    pub fn required_dimensions(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (d, on) in [
            (dim::MULTI_UID, self.multi_uid),
            (dim::PRIVATE_PROC, self.private_proc),
            (dim::FULL_INIT_SYSTEM, self.full_init_system),
            (dim::HARD_MEMORY_LIMIT, self.hard_memory_limit),
            (dim::SECCOMP, self.seccomp),
            (dim::OUTBOUND_NETWORK, self.outbound_network),
            (dim::NET_ADMIN, self.net_admin),
        ] {
            if on {
                out.push(d);
            }
        }
        out
    }
}

/// Measured capability offer of ONE backend on ONE host. Produced by
/// host-level probing / spike-derived facts; consumed by [`plan`]. "Offer"
/// not "promise": every dimension is an observation with a [`Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub kind: BackendKind,
    /// Diagnostics only (e.g. "runc 1.4.x", "linux 6.19.14 ARCH=um",
    /// "qemu-system-x86_64 tcg"). Never a selection key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    /// Dimension -> observed status. Any declared dimension absent here is
    /// treated as UNSUPPORTED for that backend.
    pub dimensions: BTreeMap<String, Status>,
    /// Whether the backend is usable at all on this host (binaries present,
    /// minimum host mechanisms proven — e.g. userns for Native, successful
    /// test-boot for Kernel). An unavailable backend is never selected.
    pub available: bool,
    #[serde(default)]
    pub note: String,
}

impl BackendCapabilities {
    pub fn status_of(&self, dimension: &str) -> Status {
        self.dimensions
            .get(dimension)
            .copied()
            .unwrap_or(Status::Unsupported)
    }

    fn with_dims(
        kind: BackendKind,
        available: bool,
        implementation: Option<String>,
        dims: [(String, Status); dim::ALL.len()],
        note: &str,
    ) -> Self {
        BackendCapabilities {
            kind,
            implementation,
            dimensions: dims.into_iter().collect::<BTreeMap<_, _>>(),
            available,
            note: note.to_string(),
        }
    }

    /// The Native (runc) offer mapped from the T41 dimensioned host
    /// capability observation. Source of truth stays the measured
    /// HostCapabilities; this is a view, not a second probe.
    pub fn native(host: &HostCapabilities, implementation: Option<String>) -> Self {
        fn bool_str_supported(v: &str, yes: &str) -> Status {
            if v == yes {
                Status::Supported
            } else {
                Status::Unsupported
            }
        }
        let multi_uid = match host.identity.uid_mapping_mode {
            UidMappingMode::MultiId => Status::Supported,
            UidMappingMode::SingleId | UidMappingMode::Unsupported => Status::Unsupported,
            UidMappingMode::NotTested => Status::NotTested,
        };
        Self::with_dims(
            BackendKind::Native,
            host.identity.user_namespace == Status::Supported,
            implementation,
            [
                (dim::MULTI_UID.into(), multi_uid),
                (
                    dim::PRIVATE_PROC.into(),
                    bool_str_supported(&host.process.proc_policy.mode, "PRIVATE_PROC"),
                ),
                // Native pid 1 IS the workload payload: no guest init-system
                // semantics exist to offer. This is a semantic fact, not a gap
                // to be patched over with wording.
                (dim::FULL_INIT_SYSTEM.into(), Status::Unsupported),
                (dim::HARD_MEMORY_LIMIT.into(), host.resource.hard_memory_limit),
                (
                    dim::SECCOMP.into(),
                    bool_str_supported(&host.process.seccomp, "AVAILABLE"),
                ),
                (dim::OUTBOUND_NETWORK.into(), host.network.outbound),
                (dim::NET_ADMIN.into(), host.network.net_admin),
            ],
            "mapped from HostCapabilities (T41 observation); no separate native probe",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedRequirement {
    pub dimension: String,
    pub backend: BackendKind,
    pub observed: Status,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Degradation {
    pub dimension: String,
    pub status: Status,
    pub reason: String,
}

/// Planner result. `effective_capabilities` lists EVERY declared dimension
/// for the selected backend (unsupported stays visible; nothing silently
/// drops), mirroring the T41 degradation_rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendPlan {
    pub selected: BackendKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    pub preference: BackendPreference,
    pub effective_capabilities: BTreeMap<String, Status>,
    pub degradations: Vec<Degradation>,
    /// Always empty on success; requirements are prerequisites, not warnings.
    pub rejected_requirements: Vec<RejectedRequirement>,
    pub reason: String,
}

/// Contract error code shared with the T41 requirement-resolution rejection:
/// same semantic, same code — a requirement that no offered backend can
/// satisfy is a host-capability rejection, priced before any create.
pub const PLAN_REJECTION_CODE: &str = "HOST_CAPABILITY_UNAVAILABLE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRejection {
    pub code: String,
    pub preference: BackendPreference,
    pub rejected_requirements: Vec<RejectedRequirement>,
    pub reason: String,
}

impl fmt::Display for PlanRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dims: Vec<&str> = self
            .rejected_requirements
            .iter()
            .map(|r| r.dimension.as_str())
            .collect();
        write!(
            f,
            "{}: no available backend satisfies required dimensions [{}]: {}",
            self.code,
            dims.join(", "),
            self.reason
        )
    }
}

impl std::error::Error for PlanRejection {}

/// Select a backend for `requirements`.
///
/// AUTO: first backend in [`BackendKind::AUTO_ORDER`] that is available and
/// observes `SUPPORTED` for every hard requirement. Explicit preference:
/// the requested backend only; if it is unavailable or does not satisfy,
/// this is a rejection (never a silent switch to another backend).
pub fn plan(
    requirements: &CapabilityRequirements,
    preference: BackendPreference,
    offers: &[BackendCapabilities],
) -> Result<BackendPlan, PlanRejection> {
    let order: &[BackendKind] = match preference {
        BackendPreference::Auto => &BackendKind::AUTO_ORDER,
        BackendPreference::Native => &[BackendKind::Native],
        BackendPreference::Kernel => &[BackendKind::Kernel],
        BackendPreference::Machine => &[BackendKind::Machine],
    };
    let required = requirements.required_dimensions();
    let mut rejected: Vec<RejectedRequirement> = Vec::new();
    let mut unavailable: Vec<BackendKind> = Vec::new();

    for &kind in order {
        let Some(offer) = offers.iter().find(|o| o.kind == kind && o.available) else {
            unavailable.push(kind);
            continue;
        };
        let missing: Vec<RejectedRequirement> = required
            .iter()
            .filter(|d| offer.status_of(d) != Status::Supported)
            .map(|d| RejectedRequirement {
                dimension: (*d).to_string(),
                backend: kind,
                observed: offer.status_of(d),
                reason: format!(
                    "required {d} is {} on the {} backend",
                    offer.status_of(d).wire(),
                    kind.wire()
                ),
            })
            .collect();
        if !missing.is_empty() {
            rejected.extend(missing);
            continue;
        }
        // Selecting the lightest satisfying backend. Degraded non-required
        // dimensions are disclosed, not hidden.
        let degradations: Vec<Degradation> = offer
            .dimensions
            .iter()
            .filter(|(_, s)| **s == Status::Degraded)
            .map(|(d, s)| Degradation {
                dimension: d.clone(),
                status: *s,
                reason: format!("{} observes DEGRADED on the selected backend", d),
            })
            .collect();
        return Ok(BackendPlan {
            selected: kind,
            implementation: offer.implementation.clone(),
            preference,
            effective_capabilities: offer.dimensions.clone(),
            degradations,
            rejected_requirements: Vec::new(),
            reason: format!(
                "lightest available backend satisfying all {} required dimension(s)",
                required.len()
            ),
        });
    }

    let reason = match preference {
        BackendPreference::Auto => format!(
            "AUTO order NATIVE→KERNEL→MACHINE exhausted; unavailable: [{}]; unsatisfied requirements recorded per backend",
            unavailable
                .iter()
                .map(|k| k.wire())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        pref => {
            let pname = match pref {
                BackendPreference::Native => "native",
                BackendPreference::Kernel => "kernel",
                BackendPreference::Machine => "machine",
                BackendPreference::Auto => "auto",
            };
            format!(
                "explicit preference {} requested{}",
                pname,
                if unavailable.is_empty() {
                    " but it does not satisfy the requirements"
                } else {
                    " but that backend is unavailable on this host"
                }
            )
        }
    };
    Err(PlanRejection {
        code: PLAN_REJECTION_CODE.to_string(),
        preference,
        rejected_requirements: rejected,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(pairs: &[(&str, Status)]) -> BTreeMap<String, Status> {
        pairs.iter().map(|(d, s)| (d.to_string(), *s)).collect()
    }

    /// PORTABLE_ROOTLESS native shape, matching this dev worker's measured
    /// HostCapabilities: single-ID, no private proc, no cgroup delegation,
    /// seccomp available (ADR 0001 A3), no outbound net.
    fn native_portable() -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Native,
            implementation: Some("runc 1.4.x (pinned)".into()),
            dimensions: dims(&[
                (dim::MULTI_UID, Status::Unsupported),
                (dim::PRIVATE_PROC, Status::Unsupported),
                (dim::FULL_INIT_SYSTEM, Status::Unsupported),
                (dim::HARD_MEMORY_LIMIT, Status::Unsupported),
                (dim::SECCOMP, Status::Supported),
                (dim::OUTBOUND_NETWORK, Status::Unsupported),
                (dim::NET_ADMIN, Status::Unsupported),
            ]),
            available: true,
            note: String::new(),
        }
    }

    fn kernel_full() -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Kernel,
            implementation: Some("linux ARCH=um (spike)".into()),
            dimensions: dims(&[
                (dim::MULTI_UID, Status::Supported),
                (dim::PRIVATE_PROC, Status::Supported),
                (dim::FULL_INIT_SYSTEM, Status::Supported),
                (dim::HARD_MEMORY_LIMIT, Status::Degraded),
                (dim::SECCOMP, Status::NotTested),
                (dim::OUTBOUND_NETWORK, Status::NotTested),
                (dim::NET_ADMIN, Status::Supported),
            ]),
            available: true,
            note: String::new(),
        }
    }

    #[test]
    fn auto_picks_native_when_nothing_required() {
        let p = plan(
            &CapabilityRequirements::default(),
            BackendPreference::Auto,
            &[native_portable()],
        )
        .unwrap();
        assert_eq!(p.selected, BackendKind::Native);
        assert!(p.rejected_requirements.is_empty());
        // every declared dimension stays visible, even when UNSUPPORTED
        assert_eq!(p.effective_capabilities.len(), dim::ALL.len());
    }

    #[test]
    fn auto_skips_native_to_kernel_for_multi_uid() {
        let reqs = CapabilityRequirements {
            multi_uid: true,
            ..Default::default()
        };
        let p = plan(&reqs, BackendPreference::Auto, &[native_portable(), kernel_full()])
            .unwrap();
        assert_eq!(p.selected, BackendKind::Kernel);
    }

    #[test]
    fn auto_skips_to_kernel_for_full_init_system() {
        let reqs = CapabilityRequirements {
            full_init_system: true,
            ..Default::default()
        };
        let p = plan(&reqs, BackendPreference::Auto, &[native_portable(), kernel_full()])
            .unwrap();
        assert_eq!(p.selected, BackendKind::Kernel);
    }

    #[test]
    fn explicit_native_does_not_silently_switch_backend() {
        let reqs = CapabilityRequirements {
            multi_uid: true,
            ..Default::default()
        };
        let err = plan(
            &reqs,
            BackendPreference::Native,
            &[native_portable(), kernel_full()],
        )
        .unwrap_err();
        assert_eq!(err.code, PLAN_REJECTION_CODE);
        assert_eq!(err.preference, BackendPreference::Native);
        assert!(
            err.rejected_requirements
                .iter()
                .any(|r| r.dimension == dim::MULTI_UID && r.backend == BackendKind::Native)
        );
    }

    #[test]
    fn explicit_unavailable_backend_rejects() {
        let err = plan(
            &CapabilityRequirements::default(),
            BackendPreference::Kernel,
            &[native_portable()],
        )
        .unwrap_err();
        assert_eq!(err.code, PLAN_REJECTION_CODE);
        assert!(err.reason.contains("unavailable"));
    }

    #[test]
    fn auto_rejection_records_every_unsatisfied_requirement() {
        let reqs = CapabilityRequirements {
            multi_uid: true,
            hard_memory_limit: true,
            ..Default::default()
        };
        let err = plan(&reqs, BackendPreference::Auto, &[native_portable()]).unwrap_err();
        let dims: Vec<&str> = err
            .rejected_requirements
            .iter()
            .map(|r| r.dimension.as_str())
            .collect();
        assert!(dims.contains(&dim::MULTI_UID));
        assert!(dims.contains(&dim::HARD_MEMORY_LIMIT));
    }

    #[test]
    fn degraded_dimensions_are_disclosed_on_selection() {
        let p = plan(
            &CapabilityRequirements::default(),
            BackendPreference::Auto,
            &[kernel_full()], // native unavailable here
        )
        .unwrap();
        assert_eq!(p.selected, BackendKind::Kernel);
        assert!(
            p.degradations
                .iter()
                .any(|d| d.dimension == dim::HARD_MEMORY_LIMIT && d.status == Status::Degraded)
        );
    }

    #[test]
    fn native_offer_maps_from_t41_host_capabilities() {
        let host: HostCapabilities = serde_json::from_value(serde_json::json!({
            "tier": "PORTABLE_ROOTLESS",
            "identity": {
                "user_namespace": "SUPPORTED",
                "uid_mapping_mode": "SINGLE_ID",
                "gid_mapping_mode": "SINGLE_ID",
                "ownership_model": "FLATTENED_BY_UNPACK",
                "image_user_support": "UID0_ONLY"
            },
            "process": {
                "pid_namespace": "SUPPORTED",
                "proc_policy": {"mode": "HOST_RBIND_RO", "confidentiality": "METADATA_AND_ARGV_VISIBLE"},
                "seccomp": "AVAILABLE"
            },
            "resource": {
                "cgroup_delegated": "UNSUPPORTED",
                "hard_memory_limit": "UNSUPPORTED",
                "hard_cpu_limit": "UNSUPPORTED",
                "hard_pid_limit": "UNSUPPORTED"
            },
            "filesystem": {
                "rootfs_driver": "REFLINK_VOLATILE",
                "persistent_cow": "UNSUPPORTED",
                "ephemeral_cow": "SUPPORTED"
            },
            "network": {
                "namespace": "SUPPORTED",
                "outbound": "UNSUPPORTED",
                "inbound": "UNSUPPORTED",
                "raw_socket": "UNSUPPORTED",
                "net_admin": "UNSUPPORTED"
            },
            "runtime": {
                "runc": {"version": "1.4.x", "sha256": "aa", "seccomp": false},
                "supervisor": "NONE"
            }
        }))
        .unwrap();
        let offer = BackendCapabilities::native(&host, Some("runc 1.4.x".into()));
        assert!(offer.available);
        assert_eq!(offer.status_of(dim::MULTI_UID), Status::Unsupported);
        assert_eq!(offer.status_of(dim::PRIVATE_PROC), Status::Unsupported);
        assert_eq!(offer.status_of(dim::SECCOMP), Status::Supported);
        assert_eq!(offer.status_of(dim::FULL_INIT_SYSTEM), Status::Unsupported);
    }

    #[test]
    fn wire_names_are_implementation_free() {
        assert_eq!(
            serde_json::to_string(&BackendPreference::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&BackendKind::Kernel).unwrap(),
            "\"KERNEL\""
        );
        assert_eq!(
            serde_json::to_string(&HostKind::Containerized).unwrap(),
            "\"CONTAINERIZED\""
        );
    }

    #[test]
    fn full_init_system_requirement_serializes_under_its_name() {
        let reqs = CapabilityRequirements {
            full_init_system: true,
            ..Default::default()
        };
        assert_eq!(reqs.required_dimensions(), vec![dim::FULL_INIT_SYSTEM]);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&reqs).unwrap()).unwrap();
        assert_eq!(v["full_init_system"], serde_json::json!(true));
        assert_eq!(v["multi_uid"], serde_json::json!(false));
    }
}
