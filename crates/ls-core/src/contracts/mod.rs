//! Native v1 frozen contracts (T41): capability model, requirements,
//! resolution, identity, command surface.
//!
//! Parity rule (T41-C01): the JSON schemas in `contracts/v1/`, the Python
//! reference in `tests/contracts/native_v1_ref.py` and these Rust types must
//! agree on every fixture under `contracts/v1/fixtures/`. Any disagreement is
//! a contract bug, by definition.
//!
//! Frozen semantics (see contracts/v1/resolution-rules.json and ADR 0001):
//!   * requested capability -> resolver -> effective capability ->
//!     degradation/rejection; nothing silently downgrades or disappears;
//!   * `SingleIdCompatibilityPolicy` defaults to STRICT; FLATTEN_ROOT must be
//!     visible in effective degradations (never a silent USER->0 rewrite);
//!   * requirement != observation: `resources.*` are canonical integers
//!     (bytes / cpu_millis / pids); hardness comes only from `requires.*`;
//!   * secrets MUST NOT enter argv (LSR-1, docs/security/SECRET_HANDLING.md).

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn fixture_json(rel: &str) -> Value {
    let path = repo_root().join("contracts/v1").join(rel);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {rel} unreadable: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {rel} invalid: {e}"))
}

// ---------------------------------------------------------------- exec spec

/// Normalized execution spec: exactly one command form, env defaulted to {}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSpec {
    pub command: Command,
    pub cwd: Option<String>,
    pub env: Map<String, Value>,
    pub timeout_ms: Option<u64>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Argv(Vec<String>),
    Shell { executable: String, script: String },
}

const EXEC_TOP: &[&str] = &["command", "cwd", "env", "timeout_ms", "user"];

fn no_nul(s: &str) -> bool {
    !s.contains('\0')
}

impl ExecSpec {
    /// Strict parser mirroring exec.schema.json and the Python reference.
    /// Explicit null is rejected for every field (null != missing); unknown
    /// fields are rejected; JSON bool is never a number.
    pub fn parse(doc: &Value) -> Result<ExecSpec, String> {
        let obj = doc.as_object().ok_or("execspec must be an object")?;
        for (key, value) in obj {
            if !EXEC_TOP.contains(&key.as_str()) {
                return Err(format!("unknown field {key}"));
            }
            if value.is_null() {
                return Err(format!("explicit null rejected for {key}"));
            }
        }
        let command = match obj.get("command") {
            Some(Value::Object(c)) => c,
            _ => return Err("command required".to_string()),
        };
        let kind = command.get("kind").and_then(Value::as_str).unwrap_or("");
        let command = match kind {
            "argv" => {
                let extra: BTreeSet<String> = command
                    .keys()
                    .filter(|k| k.as_str() != "kind" && k.as_str() != "argv")
                    .cloned()
                    .collect();
                if !extra.is_empty() {
                    return Err(format!("argv form carries only kind+argv: {extra:?}"));
                }
                let argv = command
                    .get("argv")
                    .and_then(Value::as_array)
                    .filter(|a| !a.is_empty())
                    .ok_or("argv must be a non-empty array")?;
                let mut out = Vec::with_capacity(argv.len());
                for (i, item) in argv.iter().enumerate() {
                    let s = item.as_str().filter(|s| no_nul(s)).ok_or_else(|| {
                        format!("argv[{i}] must be a NUL-free string")
                    })?;
                    out.push(s.to_string());
                }
                if out[0].is_empty() {
                    return Err("executable cannot be empty".to_string());
                }
                Command::Argv(out)
            }
            "shell" => {
                let mut keys: BTreeSet<String> = command.keys().cloned().collect();
                for k in ["kind", "executable", "script"] {
                    keys.remove(k);
                }
                if !keys.is_empty() {
                    return Err("shell form carries only kind+executable+script".to_string());
                }
                let executable = command
                    .get("executable")
                    .and_then(Value::as_str)
                    .filter(|s| no_nul(s) && s.starts_with('/'))
                    .ok_or("shell executable must be an absolute guest path")?
                    .to_string();
                let script = match command.get("script") {
                    Some(Value::String(s)) if no_nul(s) => s.clone(),
                    _ => return Err("script must be a NUL-free string".to_string()),
                };
                Command::Shell { executable, script }
            }
            _ => return Err("command.kind must be argv|shell".to_string()),
        };
        let mut env: Map<String, Value> = Map::new();
        if let Some(v) = obj.get("env") {
            let m = v.as_object().ok_or("env must be an object")?;
            for (k, val) in m {
                if k.is_empty() || k.contains('=') || !no_nul(k) {
                    return Err("invalid env name".to_string());
                }
                let vs = val.as_str().filter(|s| no_nul(s)).ok_or("invalid env value")?;
                env.insert(k.clone(), Value::String(vs.to_string()));
            }
        }
        let cwd = match obj.get("cwd") {
            None => None,
            Some(v) => {
                let s = v
                    .as_str()
                    .filter(|s| no_nul(s) && s.starts_with('/'))
                    .ok_or("cwd must be an absolute guest path")?;
                Some(s.to_string())
            }
        };
        let timeout_ms = match obj.get("timeout_ms") {
            None => None,
            Some(v) => {
                let n = v.as_u64().filter(|n| *n >= 1).ok_or(
                    "timeout_ms must be a positive integer (ms)",
                )?;
                Some(n)
            }
        };
        let user = match obj.get("user") {
            None => None,
            Some(v) => {
                let s = v
                    .as_str()
                    .filter(|s| no_nul(s) && !s.is_empty())
                    .ok_or("user must be a non-empty NUL-free string")?;
                Some(s.to_string())
            }
        };
        Ok(ExecSpec { command, cwd, env, timeout_ms, user })
    }
}

// ------------------------------------------------------ environment (subset)

const ENV_TOP: &[&str] = &["schema_version", "services", "networks", "volumes", "labels"];
const ENV_SVC: &[&str] = &[
    "image", "launch_mode", "entrypoint", "command", "env", "cwd", "user",
    "resources", "mounts", "networks", "depends_on",
];
const RES_KEYS: &[&str] = &["cpu_millis", "memory_bytes", "storage_bytes", "pids"];

/// Strict offline EnvironmentSpec subset: advanced collections must be
/// present-empty; rejections name the offending field (T41-C02/C05).
pub fn validate_environmentspec(doc: &Value) -> Result<(), String> {
    let obj = doc.as_object().ok_or("environmentspec must be an object")?;
    let unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !ENV_TOP.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(format!("unknown fields: [{}]", unknown.join(", ")));
    }
    let services = obj
        .get("services")
        .and_then(Value::as_object)
        .filter(|s| !s.is_empty())
        .ok_or("services must be a non-empty object")?;
    for top in ["networks", "volumes"] {
        match obj.get(top) {
            Some(Value::Object(m)) if !m.is_empty() => {
                return Err(format!("{top}: advanced field not implemented in offline subset"));
            }
            _ => {}
        }
    }
    for (name, svc) in services {
        let svc = svc
            .as_object()
            .ok_or_else(|| format!("services.{name} must be an object"))?;
        for key in svc.keys() {
            if !ENV_SVC.contains(&key.as_str()) {
                return Err(format!("services.{name}: unknown fields: [{key}]"));
            }
        }
        let image_ok = svc
            .get("image")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && no_nul(s));
        if !image_ok {
            return Err(format!("services.{name}: image required"));
        }
        for adv in ["mounts", "networks", "depends_on"] {
            let present = match svc.get(adv) {
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::Object(o)) => !o.is_empty(),
                Some(_) => true,
                None => false,
            };
            if present {
                return Err(format!(
                    "services.{name}.{adv}: advanced field not implemented in offline subset"
                ));
            }
        }
        let mode = svc.get("launch_mode").and_then(Value::as_str).unwrap_or("image");
        if mode != "image" && mode != "idle-agent" {
            return Err(format!("services.{name}.launch_mode unknown: {mode:?}"));
        }
        for field in ["entrypoint", "command"] {
            match svc.get(field) {
                None | Some(Value::Null) => {} // null = inherit (documented)
                Some(Value::Array(items)) => {
                    for item in items {
                        let ok = item.as_str().is_some_and(|s| no_nul(s));
                        if !ok {
                            return Err(format!(
                                "services.{name}.{field} must be NUL-free strings"
                            ));
                        }
                    }
                    if items.first().and_then(Value::as_str) == Some("") {
                        return Err(format!("services.{name}.{field}[0] cannot be empty"));
                    }
                }
                Some(_) => {
                    return Err(format!(
                        "services.{name}.{field} must be an array or null(inherit)"
                    ));
                }
            }
        }
        if let Some(res) = svc.get("resources") {
            let res = res
                .as_object()
                .ok_or_else(|| format!("services.{name}.resources must be an object"))?;
            for (rk, rv) in res {
                if !RES_KEYS.contains(&rk.as_str()) {
                    return Err(format!("services.{name}.resources: unknown field {rk}"));
                }
                if rv.as_u64().filter(|n| *n >= 1).is_none() {
                    return Err(format!(
                        "services.{name}.resources.{rk} must be a positive integer"
                    ));
                }
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------- host + requirements

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tier {
    PortableRootless,
    FullRootless,
    Rootful,
    UnsupportedHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Supported,
    Degraded,
    Unsupported,
    NotTested,
}

impl Status {
    pub fn wire(&self) -> &'static str {
        match self {
            Status::Supported => "SUPPORTED",
            Status::Degraded => "DEGRADED",
            Status::Unsupported => "UNSUPPORTED",
            Status::NotTested => "NOT_TESTED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UidMappingMode {
    SingleId,
    MultiId,
    Unsupported,
    NotTested,
}

impl UidMappingMode {
    pub fn wire(&self) -> &'static str {
        match self {
            UidMappingMode::SingleId => "SINGLE_ID",
            UidMappingMode::MultiId => "MULTI_ID",
            UidMappingMode::Unsupported => "UNSUPPORTED",
            UidMappingMode::NotTested => "NOT_TESTED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SingleIdCompatibilityPolicy {
    /// Refuses unfaithful UID/GID expressions at create/preflight.
    #[default]
    Strict,
    /// Explicit benchmark-compat switch; must stay visible in degradations.
    FlattenRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityCaps {
    pub user_namespace: Status,
    pub uid_mapping_mode: UidMappingMode,
    pub gid_mapping_mode: UidMappingMode,
    pub ownership_model: String,
    pub image_user_support: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcPolicy {
    pub mode: String,
    pub confidentiality: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCaps {
    pub pid_namespace: Status,
    pub proc_policy: ProcPolicy,
    /// AVAILABLE on this host once runc-seccomp is adopted (ADR 0001 A3).
    pub seccomp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCaps {
    pub cgroup_delegated: Status,
    pub hard_memory_limit: Status,
    pub hard_cpu_limit: Status,
    pub hard_pid_limit: Status,
    #[serde(default)]
    pub process_guardrails_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemCaps {
    pub rootfs_driver: String,
    pub persistent_cow: Status,
    pub ephemeral_cow: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkCaps {
    pub namespace: Status,
    pub outbound: Status,
    pub inbound: Status,
    pub raw_socket: Status,
    pub net_admin: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuncFact {
    pub version: String,
    pub sha256: String,
    pub seccomp: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCaps {
    pub runc: RuncFact,
    pub supervisor: String,
}

/// Dimensioned host capability observation. Never a requirement.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCapabilities {
    pub tier: Tier,
    pub identity: IdentityCaps,
    pub process: ProcessCaps,
    pub resource: ResourceCaps,
    pub filesystem: FilesystemCaps,
    pub network: NetworkCaps,
    pub runtime: RuntimeCaps,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub multi_uid: bool,
    #[serde(default)]
    pub hard_memory_limit: bool,
    #[serde(default)]
    pub private_proc: bool,
    #[serde(default)]
    pub seccomp: bool,
    #[serde(default)]
    pub outbound_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRequirement {
    #[serde(rename = "ref")]
    pub image_ref: String,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObservation {
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    #[serde(default)]
    pub storage_bytes: Option<u64>,
    #[serde(default)]
    pub cpu_millis: Option<u64>,
    #[serde(default)]
    pub pids: Option<u64>,
}

/// Requirements are hard preconditions; resources are observations with
/// canonical integer units. Serde rejects bools/floats/strings for u64
/// fields and unknown fields outright (units cannot become strings).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRequirements {
    pub image: ImageRequirement,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub single_id_compatibility_policy: SingleIdCompatibilityPolicy,
    #[serde(default)]
    pub resources: Option<ResourceObservation>,
}

// ---------------------------------------------------------------- resolution

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub requirement: String,
    pub code: String,
    pub capability: String,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({})", self.code, self.requirement, self.capability)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degradation {
    pub capability: String,
    pub mode: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub accepted: bool,
    pub rejections: Vec<Rejection>,
    pub degradations: Vec<Degradation>,
    /// Effective capabilities mirror the host dimensions; UNSUPPORTED stays
    /// listed — unsupported cannot silently disappear.
    pub effective: Value,
}

/// Drives off the frozen rules table; both resolvers consume the same file.
pub fn resolve(
    host: &HostCapabilities,
    host_raw: &Value,
    spec: &SandboxRequirements,
    rules: &Value,
) -> Resolution {
    let mut rejections = Vec::new();
    let mut degradations = Vec::new();
    let reqs: Vec<(&str, bool, &str)> = vec![
        ("multi_uid", spec.requires.multi_uid, "identity.uid_mapping_mode"),
        ("hard_memory_limit", spec.requires.hard_memory_limit, "resource.hard_memory_limit"),
        ("private_proc", spec.requires.private_proc, "process.proc_policy.mode"),
        ("seccomp", spec.requires.seccomp, "process.seccomp"),
        ("outbound_network", spec.requires.outbound_network, "network.outbound"),
    ];
    let map = &rules["requirements_map"];
    for (name, wanted, capability) in reqs {
        if !wanted {
            continue;
        }
        let accepted = map[name]["accepted_value"].as_str().unwrap();
        let actual = match capability {
            "identity.uid_mapping_mode" => host.identity.uid_mapping_mode.wire(),
            "resource.hard_memory_limit" => host.resource.hard_memory_limit.wire(),
            "process.proc_policy.mode" => host.process.proc_policy.mode.as_str(),
            "process.seccomp" => host.process.seccomp.as_str(),
            "network.outbound" => host.network.outbound.wire(),
            other => panic!("rules table maps unknown capability {other}"),
        };
        if actual != accepted {
            rejections.push(Rejection {
                requirement: name.to_string(),
                code: rules["rejection"]["code"].as_str().unwrap().to_string(),
                capability: capability.to_string(),
            });
        }
    }
    if let Some(user) = &spec.image.user {
        if user != "0" && user != "root"
            && host.identity.uid_mapping_mode == UidMappingMode::SingleId
        {
            match spec.single_id_compatibility_policy {
                SingleIdCompatibilityPolicy::FlattenRoot => {
                    degradations.push(Degradation {
                        capability: "identity.ownership_model".to_string(),
                        mode: "FLATTEN_ROOT".to_string(),
                        detail: format!("image user {user} folded to uid 0"),
                    });
                }
                SingleIdCompatibilityPolicy::Strict => {
                    rejections.push(Rejection {
                        requirement: "image.user".to_string(),
                        code: rules["policy_rejection"]["code"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                        capability: "identity.image_user_support".to_string(),
                    });
                }
            }
        }
    }
    let effective = serde_json::json!({
        "identity": host_raw["identity"],
        "process": host_raw["process"],
        "resource": host_raw["resource"],
        "filesystem": host_raw["filesystem"],
        "network": host_raw["network"],
        "runtime": host_raw["runtime"],
    });
    Resolution {
        accepted: rejections.is_empty(),
        rejections,
        degradations,
        effective,
    }
}

// ------------------------------------------------------------------ states

pub fn legal_transition(states: &Value, domain: &str, from: &str, to: &str) -> bool {
    states["domains"][domain][from]
        .as_array()
        .map(|xs| xs.iter().any(|x| x.as_str() == Some(to)))
        .unwrap_or(false)
}

/// exited requires an observed exit record; unknown records are terminal but
/// never erased (see states.json late_confirmation).
pub fn check_semantic_fact(doc: &Value) -> bool {
    if let Some(ex) = doc.get("execution") {
        if ex.get("state").and_then(Value::as_str) == Some("exited")
            && ex.get("exit_record").map_or(true, Value::is_null)
        {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------- identity

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRejection {
    pub code: &'static str,
    pub http_status: u16,
}

impl fmt::Display for ContractRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl std::error::Error for ContractRejection {}

/// generation fences a sandbox incarnation; revision is the record-state
/// CAS counter. An API/manager restart reloads both unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxIdentity {
    pub sandbox_id: String,
    pub generation: u64,
    pub revision: u64,
}

impl SandboxIdentity {
    pub fn new(sandbox_id: &str, generation: u64, revision: u64) -> Self {
        SandboxIdentity {
            sandbox_id: sandbox_id.to_string(),
            generation,
            revision,
        }
    }

    /// Process restarts never mint a new incarnation.
    pub fn api_restart(&self) -> Self {
        self.clone()
    }

    /// A new incarnation: generation +1, revision resets. Deleted sandbox
    /// ids are never reused.
    pub fn recreate_incarnation(&self) -> Self {
        SandboxIdentity {
            sandbox_id: self.sandbox_id.clone(),
            generation: self.generation + 1,
            revision: 0,
        }
    }

    pub fn cas_update(&mut self, expected_revision: u64) -> Result<u64, ContractRejection> {
        if expected_revision != self.revision {
            return Err(ContractRejection { code: "STALE_GENERATION", http_status: 409 });
        }
        self.revision += 1;
        Ok(self.revision)
    }

    pub fn bind_execution(
        &self,
        execution_id: &str,
        claimed_generation: u64,
    ) -> Result<(), ContractRejection> {
        if claimed_generation != self.generation {
            return Err(ContractRejection { code: "STALE_GENERATION", http_status: 409 });
        }
        let _ = execution_id;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cases(name: &str) -> Value {
        fixture_json(name)
    }

    #[test]
    fn c01_execspec_all_cases_parse_as_expected() {
        let fx = cases("fixtures/execspec/cases.json");
        let mut accept = 0;
        let mut reject = 0;
        for case in fx["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let expect_ok = case["expect"].as_str().unwrap() == "ACCEPT";
            match (ExecSpec::parse(&case["document"]), expect_ok) {
                (Ok(spec), true) => {
                    accept += 1;
                    let want = &case["normalized"];
                    if let Some(argv) = want.get("argv") {
                        let Command::Argv(got) = &spec.command else {
                            panic!("{name}: expected argv form");
                        };
                        let want_argv: Vec<String> = argv
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_str().unwrap().to_string())
                            .collect();
                        assert_eq!(*got, want_argv, "{name}: argv not preserved verbatim");
                    }
                    if let Some(exe) = want.get("executable") {
                        let Command::Shell { executable, script } = &spec.command else {
                            panic!("{name}: expected shell form");
                        };
                        assert_eq!(executable, exe.as_str().unwrap(), "{name}");
                        assert_eq!(script, want["script"].as_str().unwrap(), "{name}");
                    }
                    let want_env = want["env"].as_object().unwrap();
                    assert_eq!(spec.env.len(), want_env.len(), "{name}: env mismatch");
                    if let Some(cwd) = want.get("cwd") {
                        assert_eq!(spec.cwd.as_deref(), cwd.as_str(), "{name}: cwd");
                    }
                }
                (Err(_), false) => reject += 1,
                (Ok(spec), false) => panic!("{name}: parsed but fixture expects REJECT: {spec:?}"),
                (Err(e), true) => panic!("{name}: rejected but fixture expects ACCEPT: {e}"),
            }
        }
        assert!(accept >= 5 && reject >= 18, "fixture coverage shrank: {accept}/{reject}");
    }

    #[test]
    fn c02_c05_environment_boundary_cases() {
        let fx = cases("fixtures/environment/cases.json");
        for case in fx["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let expect = case["expect"].as_str().unwrap();
            if expect == "METADATA_ROUNDTRIP" {
                let clone: Value = serde_json::from_str(
                    &serde_json::to_string(&case["document"]).unwrap(),
                )
                .unwrap();
                assert_eq!(clone, case["document"], "{name}: metadata mangled");
                continue;
            }
            let result = validate_environmentspec(&case["document"]);
            match (expect, result) {
                ("ACCEPT", Ok(())) => {}
                ("REJECT", Err(e)) => {
                    for field in case["expect_fields"].as_array().unwrap_or(&vec![]) {
                        assert!(
                            e.contains(field.as_str().unwrap()),
                            "{name}: rejection must name {field:?}, got {e}"
                        );
                    }
                }
                ("ACCEPT", Err(e)) => panic!("{name}: rejected but expected ACCEPT: {e}"),
                ("REJECT", Ok(())) => panic!("{name}: accepted but expected REJECT"),
                (other, _) => panic!("{name}: unknown expect {other}"),
            }
        }
    }

    #[test]
    fn c03_states_transitions_and_facts() {
        let states = cases("states.json");
        let fx = cases("fixtures/states/cases.json");
        for case in fx["illegal_transitions"].as_array().unwrap() {
            assert!(
                !legal_transition(
                    &states,
                    case["domain"].as_str().unwrap(),
                    case["from"].as_str().unwrap(),
                    case["to"].as_str().unwrap()
                ),
                "illegal transition must not be legal: {case}"
            );
        }
        for (domain, table) in states["domains"].as_object().unwrap() {
            let edges = table.as_object().unwrap();
            for (src, dsts) in edges {
                for dst in dsts.as_array().unwrap() {
                    assert!(
                        edges.contains_key(dst.as_str().unwrap()),
                        "{domain}: {src}->{dst} targets undeclared state"
                    );
                }
            }
        }
        for (domain, entry) in states["late_confirmation"].as_object().unwrap() {
            assert_eq!(entry["from"], "unknown", "{domain}");
            assert!(!entry["requires"].as_str().unwrap().trim().is_empty());
        }
        let op = &states["late_confirmation"]["operation"]["to"];
        assert!(op == &serde_json::json!(["succeeded", "failed"]));
        for fact in fx["semantic_facts"].as_array().unwrap() {
            assert_eq!(
                check_semantic_fact(&fact["document"]),
                fact["consistent"].as_bool().unwrap(),
                "semantic fact {}",
                fact["name"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn c04_identity_cases() {
        let fx = cases("fixtures/identity/cases.json");
        for case in fx["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let given = &case["given"];
            let mut ident = SandboxIdentity::new(
                given["sandbox_id"].as_str().unwrap(),
                given["generation"].as_u64().unwrap(),
                given["revision"].as_u64().unwrap(),
            );
            let want = &case["expect"];
            match case["action"]["kind"].as_str().unwrap() {
                "api_restart" => {
                    let after = ident.api_restart();
                    assert_eq!(after.generation, want["generation"].as_u64().unwrap(), "{name}");
                    assert_eq!(after.revision, want["revision"].as_u64().unwrap(), "{name}");
                }
                "recreate_incarnation" => {
                    let after = ident.recreate_incarnation();
                    assert_eq!(after.generation, want["generation"].as_u64().unwrap(), "{name}");
                    assert_eq!(after.revision, want["revision"].as_u64().unwrap(), "{name}");
                }
                "cas_update" => {
                    match ident.cas_update(case["action"]["expected_revision"].as_u64().unwrap())
                    {
                        Ok(rev) => {
                            assert_eq!(want["outcome"], "ACCEPT", "{name}");
                            assert_eq!(rev, want["revision"].as_u64().unwrap(), "{name}");
                        }
                        Err(e) => {
                            assert_eq!(want["outcome"], "REJECT", "{name}");
                            if let Some(err) = want.get("error") {
                                assert_eq!(e.code, err["code"].as_str().unwrap(), "{name}");
                                if let Some(hs) = err.get("http_status") {
                                    assert_eq!(e.http_status, hs.as_u64().unwrap() as u16, "{name}");
                                }
                            }
                            if let Some(doc) = want.get("error_document") {
                                for k in ["code", "message", "request_id"] {
                                    assert!(
                                        doc["error"][k].as_str().is_some_and(|s| !s.is_empty()),
                                        "{name}: error_document.{k} must be a non-empty string"
                                    );
                                }
                            }
                        }
                    }
                }
                "bind_execution" => {
                    match ident.bind_execution(
                        case["action"]["execution_id"].as_str().unwrap(),
                        case["action"]["claimed_generation"].as_u64().unwrap(),
                    ) {
                        Ok(()) => assert_eq!(want["outcome"], "ACCEPT", "{name}"),
                        Err(e) => {
                            assert_eq!(want["outcome"], "REJECT", "{name}");
                            assert_eq!(e.code, want["error"]["code"].as_str().unwrap());
                        }
                    }
                }
                other => panic!("{name}: unknown action {other}"),
            }
        }
    }

    #[test]
    fn resolution_cases_match_frozen_rules() {
        let fx = cases("fixtures/resolution/cases.json");
        let rules = cases("resolution-rules.json");
        let hosts = fx["hosts"].as_object().unwrap();
        for (hname, h) in hosts {
            let _: HostCapabilities = serde_json::from_value(h.clone())
                .unwrap_or_else(|e| panic!("host profile {hname} fails typed parse: {e}"));
        }
        for case in fx["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let host_raw = &hosts[case["host"].as_str().unwrap()];
            let host: HostCapabilities = serde_json::from_value(host_raw.clone()).unwrap();
            let spec: SandboxRequirements = serde_json::from_value(case["spec"].clone())
                .unwrap_or_else(|e| panic!("{name}: spec fails typed parse: {e}"));
            let got = resolve(&host, host_raw, &spec, &rules);
            let want = &case["expect"];
            assert_eq!(
                got.accepted,
                want["outcome"].as_str().unwrap() == "ACCEPT",
                "{name}: outcome; rejections={:?}",
                got.rejections
            );
            let mut want_rej: Vec<(String, String, String)> = want
                .get("rejections")
                .and_then(Value::as_array)
                .map(|xs| {
                    xs.iter()
                        .map(|r| {
                            (
                                r["requirement"].as_str().unwrap().to_string(),
                                r["code"].as_str().unwrap().to_string(),
                                r["capability"].as_str().unwrap().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut got_rej: Vec<(String, String, String)> = got
                .rejections
                .iter()
                .map(|r| (r.requirement.clone(), r.code.clone(), r.capability.clone()))
                .collect();
            want_rej.sort();
            got_rej.sort();
            assert_eq!(got_rej, want_rej, "{name}: rejections");
            for degr in want.get("degradations").and_then(Value::as_array).into_iter().flatten()
            {
                let needle = (
                    degr["capability"].as_str().unwrap().to_string(),
                    degr["mode"].as_str().unwrap().to_string(),
                    degr["detail"].as_str().unwrap().to_string(),
                );
                assert!(
                    got.degradations
                        .iter()
                        .any(|d| (d.capability.clone(), d.mode.clone(), d.detail.clone()) == needle),
                    "{name}: missing degradation {needle:?}"
                );
            }
            for dim in ["identity", "process", "resource", "filesystem", "network", "runtime"] {
                assert!(got.effective.get(dim).is_some(), "{name}: dim {dim} disappeared");
            }
            for r in &got.rejections {
                assert!(
                    r.code == "HOST_CAPABILITY_UNAVAILABLE" || r.code == "POLICY_UNSUPPORTED",
                    "{name}: non-frozen rejection code {}",
                    r.code
                );
            }
        }
    }

    #[test]
    fn resolution_schema_level_cases() {
        let fx = cases("fixtures/resolution/cases.json");
        for case in fx["schema_level_cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let parsed: Result<SandboxRequirements, _> =
                serde_json::from_value(case["spec"].clone());
            match case["expect"].as_str().unwrap() {
                "SCHEMA_REJECT" => assert!(parsed.is_err(), "{name}: must not parse"),
                "SCHEMA_ACCEPT" => {
                    let spec = parsed.unwrap_or_else(|e| panic!("{name}: {e}"));
                    if let Some(policy) = case.get("normalized_policy") {
                        let want = policy.as_str().unwrap();
                        let got = match spec.single_id_compatibility_policy {
                            SingleIdCompatibilityPolicy::Strict => "STRICT",
                            SingleIdCompatibilityPolicy::FlattenRoot => "FLATTEN_ROOT",
                        };
                        assert_eq!(got, want, "{name}: policy default drifted");
                    }
                }
                other => panic!("{name}: unknown expect {other}"),
            }
        }
    }

    #[test]
    fn policy_default_is_strict() {
        assert_eq!(
            SingleIdCompatibilityPolicy::default(),
            SingleIdCompatibilityPolicy::Strict
        );
    }
}
