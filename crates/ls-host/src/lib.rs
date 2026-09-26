//! ls-host: host probing primitives.
//!
//! T00 scope: unprivileged, read-only probes producing capability findings
//! with bound evidence (argv form, no shell). T03 extends this module with
//! worker-authorized runtime checks; see plans/tasks/T03.md.
//! T06 adds the OCI image module (`images`): digest-verified import store
//! and image-config conversion, plans/tasks/T06.md.
//! T08 adds the runtime slice (`runtime`): the create/inspect/execute/delete
//! minimal API over a real runc backend, plans/tasks/T08.md.

pub mod exec;
pub mod images;
pub mod resources;
pub mod rootfs;
pub mod runtime;

pub mod probe {
    use ls_core::capability::{CapabilityFinding, CapabilityStatus};
    use std::process::Command;

    const EXCERPT_LIMIT: usize = 2000;

    /// Run one read-only probe command and convert the outcome into a finding.
    ///
    /// * exit 0     => `Supported`
    /// * exit != 0  => `Unsupported` (raw code preserved)
    /// * spawn error (e.g. binary missing) => `NotTested`
    pub fn command_finding(name: &str, argv: &[&str]) -> CapabilityFinding {
        assert!(!argv.is_empty(), "probe argv must not be empty");
        let cmdline = argv.join(" ");
        match Command::new(argv[0]).args(&argv[1..]).output() {
            Ok(out) => {
                let mut excerpt = String::from_utf8_lossy(&out.stdout).into_owned();
                if !out.stderr.is_empty() {
                    if !excerpt.is_empty() {
                        excerpt.push('\n');
                    }
                    excerpt.push_str(&String::from_utf8_lossy(&out.stderr));
                }
                let trimmed = excerpt.trim();
                let end = trimmed.floor_char_boundary(EXCERPT_LIMIT.min(trimmed.len()));
                CapabilityFinding {
                    name: name.to_string(),
                    status: if out.status.success() {
                        CapabilityStatus::Supported
                    } else {
                        CapabilityStatus::Unsupported
                    },
                    evidence_command: cmdline,
                    exit_code: out.status.code(),
                    output_excerpt: trimmed[..end].to_string(),
                }
            }
            Err(err) => CapabilityFinding {
                name: name.to_string(),
                status: CapabilityStatus::NotTested,
                evidence_command: cmdline,
                exit_code: None,
                output_excerpt: format!("spawn failed: {err}"),
            },
        }
    }

    /// Read a numeric sysctl-like file; value present and > 0 => `Supported`.
    pub fn positive_numeric_file_finding(name: &str, path: &str) -> CapabilityFinding {
        let evidence = format!("read {path}");
        match std::fs::read_to_string(path) {
            Ok(text) => match text.trim().parse::<i64>() {
                Ok(v) if v > 0 => {
                    CapabilityFinding::new(name, CapabilityStatus::Supported, &evidence, Some(0), text.trim())
                }
                Ok(v) => CapabilityFinding::new(
                    name,
                    CapabilityStatus::Unsupported,
                    &evidence,
                    Some(0),
                    &format!("value {v}"),
                ),
                Err(_) => CapabilityFinding::new(
                    name,
                    CapabilityStatus::Unsupported,
                    &evidence,
                    Some(0),
                    "non-numeric content",
                ),
            },
            Err(err) => CapabilityFinding::new(
                name,
                CapabilityStatus::NotTested,
                &evidence,
                None,
                &format!("read failed: {err}"),
            ),
        }
    }

    /// Unprivileged baseline probes used for smoke checks. T03 owns the full set.
    pub fn baseline_probes() -> Vec<CapabilityFinding> {
        vec![
            command_finding(ls_core::capability::USERNS_CREATE, &["unshare", "-Ur", "true"]),
            positive_numeric_file_finding(
                "kernel.max_user_namespaces",
                "/proc/sys/user/max_user_namespaces",
            ),
        ]
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ls_core::capability::CapabilityStatus;

        #[test]
        fn exit_zero_is_supported_with_exact_code() {
            let f = command_finding("fixture.true", &["/bin/true"]);
            assert_eq!(f.status, CapabilityStatus::Supported);
            assert_eq!(f.exit_code, Some(0));
            assert_eq!(f.evidence_command, "/bin/true");
        }

        #[test]
        fn nonzero_exit_is_unsupported_with_raw_code() {
            let f = command_finding("fixture.false", &["/bin/false"]);
            assert_eq!(f.status, CapabilityStatus::Unsupported);
            assert_eq!(f.exit_code, Some(1));
        }

        #[test]
        fn missing_binary_is_not_tested_no_exit_code() {
            let f = command_finding("fixture.missing", &["/definitely/not/a/binary-t00"]);
            assert_eq!(f.status, CapabilityStatus::NotTested);
            assert_eq!(f.exit_code, None);
            assert!(f.output_excerpt.contains("spawn failed"));
        }

        #[test]
        fn numeric_file_probe_uses_real_kernel_value() {
            let f = positive_numeric_file_finding(
                "kernel.max_user_namespaces",
                "/proc/sys/user/max_user_namespaces",
            );
            assert_eq!(f.status, CapabilityStatus::Supported, "{f:?}");
            let value: i64 = f.output_excerpt.parse().unwrap();
            assert!(value > 0);
        }

        #[test]
        fn real_unshare_probe_supported_on_dev_class_host() {
            let findings = baseline_probes();
            let userns = findings
                .iter()
                .find(|f| f.name == ls_core::capability::USERNS_CREATE)
                .unwrap();
            assert_eq!(
                userns.status,
                CapabilityStatus::Supported,
                "unprivileged user namespaces are required on supported dev hosts: {userns:?}"
            );
            assert_eq!(userns.exit_code, Some(0));
        }
    }
}
