"""Cross-language validation of HostCapabilityReport JSON (T00).

Mirrors the Rust ls-core::capability shapes so a report produced by the Rust
probe can be checked from Python tooling without diverging definitions.
The authoritative rule set lives in docs/v5/03_DOMAIN.md §宿主能力分层.
"""
from __future__ import annotations

TIERS = {"PORTABLE_ROOTLESS", "FULL_ROOTLESS", "ROOTFUL"}
STATUSES = {"SUPPORTED", "DEGRADED", "UNSUPPORTED", "NOT_TESTED"}

_REQUIRED_FINDING_KEYS = ("name", "status", "evidence_command")


def validate_finding(finding: dict) -> list[str]:
    errors = []
    if not isinstance(finding, dict):
        return [f"finding must be an object, got {type(finding).__name__}"]
    for key in _REQUIRED_FINDING_KEYS:
        if key not in finding:
            errors.append(f"finding missing required key {key!r}")
    if errors:
        return errors
    name = finding["name"]
    if not isinstance(name, str) or not name.strip():
        errors.append("finding name must be a non-empty string")
    status = finding["status"]
    if status not in STATUSES:
        errors.append(f"finding {name!r} has unknown status {status!r}")
    evidence = finding["evidence_command"]
    if not isinstance(evidence, str) or not evidence.strip():
        errors.append(f"finding {name!r} must bind a non-empty evidence_command")
    exit_code = finding.get("exit_code")
    if exit_code is not None and not isinstance(exit_code, int):
        errors.append(f"finding {name!r} exit_code must be an integer or null")
    if status == "SUPPORTED" and exit_code not in (0, None):
        errors.append(f"finding {name!r} claims SUPPORTED with exit_code {exit_code}")
    excerpt = finding.get("output_excerpt", "")
    if not isinstance(excerpt, str):
        errors.append(f"finding {name!r} output_excerpt must be a string")
    return errors


def validate_report(report: dict) -> list[str]:
    """Return a list of structural errors; empty means the report is well-formed."""
    errors = []
    if not isinstance(report, dict):
        return [f"report must be an object, got {type(report).__name__}"]
    probe_date = report.get("probe_date")
    if not isinstance(probe_date, str) or not probe_date.strip():
        errors.append("probe_date is required")
    hostname = report.get("hostname")
    if not isinstance(hostname, str) or not hostname.strip():
        errors.append("hostname is required")
    findings = report.get("findings")
    if not isinstance(findings, list):
        errors.append("findings must be a list")
        return errors
    seen = set()
    for index, finding in enumerate(findings):
        for err in validate_finding(finding):
            errors.append(f"findings[{index}]: {err}")
        if isinstance(finding, dict):
            name = finding.get("name")
            if name in seen:
                errors.append(f"findings[{index}]: duplicate finding name {name!r}")
            seen.add(name)
    tier = report.get("derived_tier")
    if tier is not None and tier not in TIERS:
        errors.append(f"derived_tier {tier!r} is not a known HostCapabilityTier")
    return errors
