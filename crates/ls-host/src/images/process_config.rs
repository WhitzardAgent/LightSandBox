//! Image config -> native process spec conversion (T06-C02).
//!
//! Semantics (frozen by docs/v5/03_DOMAIN.md):
//! * `launch_mode=image` keeps the image ENTRYPOINT/CMD and the image's
//!   PID-1 lifecycle;
//! * override `null`/missing = INHERIT, `[]` = CLEAR, `[..]` = REPLACE;
//! * process argv = entrypoint ++ command (the OCI/Docker rule);
//! * empty string and unset are DIFFERENT values everywhere (review focus);
//! * Docker-CLI extras (e.g. `--entrypoint` resetting Cmd) are compiled by
//!   adapters BEFORE reaching this resolver and do not appear here;
//! * the exec spec never inherits the builder/host environment: env is
//!   exactly image env + explicit overrides (docs/v5/09_RUNTIME_DETAILS.md).

use crate::images::error::{ImageError, Result};
use ls_core::domain::ids::GuestPath;
use serde_json::Value;

/// Parsed image config process fields. `None` = key absent (or OCI empty
/// default for user/working_dir/stop_signal); empty-vs-unset preserved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageProcessConfig {
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    /// Ordered `KEY=VALUE` entries exactly as the image declares them.
    pub env: Vec<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub stop_signal: Option<String>,
}

/// Tri-state native override for argv-shaped fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgvOverride {
    Inherit,
    Clear,
    Replace(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOverrides {
    pub entrypoint: Option<ArgvOverride>,
    pub command: Option<ArgvOverride>,
    /// Explicit env sets, in order: (key, value). Value may be empty
    /// (set-empty is not unset). Key removal is NOT expressible on purpose.
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    pub user: Option<String>,
}

impl ProcessOverrides {
    pub fn none() -> Self {
        Self::default()
    }
}

/// Deterministic conversion result. `argv` is derived, never authored
/// separately: entrypoint ++ command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: String,
    pub user: Option<String>,
    pub stop_signal: Option<String>,
}

fn string_list(v: &Value, field: &str) -> Result<Vec<String>> {
    let arr = v.as_array().ok_or_else(|| ImageError::LayoutInvalid {
        detail: format!("image config {field} must be an array of strings"),
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().ok_or_else(|| ImageError::LayoutInvalid {
            detail: format!("image config {field} elements must be strings"),
        })?;
        if s.contains('\0') {
            return Err(ImageError::LayoutInvalid {
                detail: format!("image config {field} element contains NUL"),
            });
        }
        out.push(s.to_string());
    }
    Ok(out)
}

/// Leading empty string is rejected in effective argv fields (mirrors the
/// T41 service validator rule: entrypoint[0]/command[0] cannot be "").
fn check_argv(items: &[String], field: &str) -> Result<()> {
    if items.first().is_some_and(|s| s.is_empty()) {
        return Err(ImageError::LayoutInvalid {
            detail: format!("EMPTY_LEADING_ARG: {field}[0] cannot be empty"),
        });
    }
    Ok(())
}

impl ImageProcessConfig {
    /// Parse the `config` object of a verified OCI image config.
    pub fn from_config(config: &Value) -> Result<Self> {
        let obj = config
            .get("config")
            .unwrap_or(&Value::Null)
            .as_object()
            .ok_or_else(|| ImageError::LayoutInvalid {
                detail: "image config .config must be an object when present".into(),
            })?;
        let entrypoint = match obj.get("Entrypoint") {
            None | Some(Value::Null) => None,
            Some(v) => Some(string_list(v, "Entrypoint")?),
        };
        let cmd = match obj.get("Cmd") {
            None | Some(Value::Null) => None,
            Some(v) => Some(string_list(v, "Cmd")?),
        };
        let env = match obj.get("Env") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => {
                let list = string_list(v, "Env")?;
                for entry in &list {
                    let (k, _) = entry.split_once('=').ok_or_else(|| {
                        ImageError::LayoutInvalid {
                            detail: format!("image config Env entry {entry:?} is not KEY=VALUE"),
                        }
                    })?;
                    if k.is_empty() {
                        return Err(ImageError::LayoutInvalid {
                            detail: "image config Env entry has empty key".into(),
                        });
                    }
                }
                list
            }
        };
        // OCI: empty WorkingDir/User/StopSignal == unset. Empty string is
        // normalised to None HERE (documented), never silently merged later.
        let working_dir = obj
            .get("WorkingDir")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(wd) = &working_dir {
            GuestPath::new(wd).map_err(|e| ImageError::LayoutInvalid {
                detail: format!("image config WorkingDir {wd:?}: {e}"),
            })?;
        }
        let user = obj
            .get("User")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(u) = &user {
            if u.contains('\0') {
                return Err(ImageError::LayoutInvalid {
                    detail: "image config User contains NUL".into(),
                });
            }
        }
        let stop_signal = obj
            .get("StopSignal")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Ok(ImageProcessConfig {
            entrypoint,
            cmd,
            env,
            working_dir,
            user,
            stop_signal,
        })
    }
}

fn resolve_argv(image: Option<&Vec<String>>, ov: Option<&ArgvOverride>, field: &str) -> Result<Vec<String>> {
    let resolved = match ov {
        None | Some(ArgvOverride::Inherit) => image.cloned().unwrap_or_default(),
        Some(ArgvOverride::Clear) => Vec::new(),
        Some(ArgvOverride::Replace(v)) => v.clone(),
    };
    check_argv(&resolved, field)?;
    Ok(resolved)
}

/// Deterministic conversion: image process config + explicit overrides ->
/// native process spec. Pure function; no host state is consulted.
pub fn convert(image: &ImageProcessConfig, ov: &ProcessOverrides) -> Result<ProcessSpec> {
    let entrypoint = resolve_argv(image.entrypoint.as_ref(), ov.entrypoint.as_ref(), "entrypoint")?;
    let command = resolve_argv(image.cmd.as_ref(), ov.command.as_ref(), "command")?;

    // env: image order preserved; override of an existing key replaces the
    // value in place; new keys append in override order. Set-empty ("K=")
    // is kept because set-empty != unset.
    let mut env = image.env.clone();
    for (k, v) in &ov.env {
        if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            return Err(ImageError::LayoutInvalid {
                detail: format!("env override key {k:?} illegal (empty, '=' or NUL)"),
            });
        }
        let prefix = format!("{k}=");
        match env.iter().position(|e| e.starts_with(&prefix)) {
            Some(i) => env[i] = format!("{k}={v}"),
            None => env.push(format!("{k}={v}")),
        }
    }

    let cwd = match &ov.cwd {
        Some(c) => {
            GuestPath::new(c).map_err(|e| ImageError::LayoutInvalid {
                detail: format!("cwd override {c:?}: {e}"),
            })?;
            c.clone()
        }
        None => image.working_dir.clone().unwrap_or_else(|| "/".to_string()),
    };

    let user = match &ov.user {
        Some(u) => {
            if u.is_empty() || u.contains('\0') {
                return Err(ImageError::LayoutInvalid {
                    detail: "user override must be a non-empty NUL-free string".into(),
                });
            }
            Some(u.clone())
        }
        None => image.user.clone(),
    };

    let argv = entrypoint
        .iter()
        .chain(command.iter())
        .cloned()
        .collect();

    Ok(ProcessSpec {
        entrypoint,
        command,
        argv,
        env,
        cwd,
        user,
        stop_signal: image.stop_signal.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MATRIX_PATH: &str = "../../tests/images/entrypoint_matrix.json";

    fn spec(entrypoint: Option<Vec<&str>>, cmd: Option<Vec<&str>>) -> ImageProcessConfig {
        ImageProcessConfig {
            entrypoint: entrypoint.map(|v| v.into_iter().map(String::from).collect()),
            cmd: cmd.map(|v| v.into_iter().map(String::from).collect()),
            ..Default::default()
        }
    }

    fn image_from_matrix(case: &Value) -> ImageProcessConfig {
        let to_list = |v: &Value| -> Option<Vec<String>> {
            match v {
                Value::Null => None,
                Value::Array(a) => Some(
                    a.iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect(),
                ),
                _ => panic!("matrix cell must be null or array"),
            }
        };
        ImageProcessConfig {
            entrypoint: to_list(&case["image"]["entrypoint"]),
            cmd: to_list(&case["image"]["cmd"]),
            ..Default::default()
        }
    }

    fn override_from_matrix(cell: &Value) -> Option<ArgvOverride> {
        match cell {
            Value::String(s) if s == "ABSENT" => None,
            Value::String(s) if s == "NULL" => Some(ArgvOverride::Inherit),
            Value::Array(a) if a.is_empty() => Some(ArgvOverride::Clear),
            Value::Array(a) => Some(ArgvOverride::Replace(
                a.iter().map(|x| x.as_str().unwrap().to_string()).collect(),
            )),
            _ => panic!("matrix override cell malformed"),
        }
    }

    fn str_list(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    }

    /// T06-C02: every matrix case passes exactly — argv, empty-vs-null
    /// distinctions, replace/clear/inherit, leading-empty rejection.
    #[test]
    fn c02_entrypoint_cmd_combination_matrix_matches_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(MATRIX_PATH);
        let doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for case in doc["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let image = image_from_matrix(case);
            let ov = ProcessOverrides {
                entrypoint: override_from_matrix(&case["override_entrypoint"]),
                command: override_from_matrix(&case["override_command"]),
                ..Default::default()
            };
            let result = convert(&image, &ov);
            if let Some(expected_err) = case.get("expect_error") {
                assert!(result.is_err(), "{name}: expected error");
                let err = result.err().unwrap();
                assert!(
                    err.to_string().contains(expected_err.as_str().unwrap()),
                    "{name}: error {err} must contain {expected_err}"
                );
                continue;
            }
            let spec = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            let expect = &case["expect"];
            assert_eq!(spec.entrypoint, str_list(&expect["entrypoint"]), "{name} entrypoint");
            assert_eq!(spec.command, str_list(&expect["command"]), "{name} command");
            assert_eq!(spec.argv, str_list(&expect["argv"]), "{name} argv");
        }
    }

    #[test]
    fn argv_is_always_derived_entrypoint_concat_command() {
        let image = spec(Some(vec!["/e", "x"]), Some(vec!["a", "b"]));
        let s = convert(&image, &ProcessOverrides::none()).unwrap();
        let mut want = vec!["/e", "x"];
        want.extend(["a", "b"]);
        assert_eq!(s.argv, want);
    }

    // ------------------------------------------------------------- env

    #[test]
    fn env_override_replaces_in_place_appends_in_order_keeps_set_empty() {
        let mut image = spec(None, None);
        image.env = vec!["A=1".into(), "B=2".into()];
        let ov = ProcessOverrides {
            env: vec![
                ("B".into(), "9".into()),
                ("C".into(), "".into()),
                ("D".into(), "seen".into()),
            ],
            ..Default::default()
        };
        let s = convert(&image, &ov).unwrap();
        assert_eq!(s.env, vec!["A=1", "B=9", "C=", "D=seen"]);
    }

    #[test]
    fn exec_spec_never_inherits_host_environment() {
        // the conversion struct has no access to std::env; env comes only
        // from image + explicit overrides (docs/v5/09_RUNTIME_DETAILS.md).
        let s = convert(&spec(None, None), &ProcessOverrides::none()).unwrap();
        assert!(s.env.is_empty());
    }

    #[test]
    fn malformed_image_env_rejected() {
        assert!(ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux",
            "config": {"Env": ["NO_EQUALS_SIGN"]}
        }))
        .is_err());
        let image = spec(None, None);
        let s = convert(&image, &ProcessOverrides {
            env: vec![("BAD=KEY".into(), "v".into())],
            ..Default::default()
        });
        assert!(s.is_err());
    }

    // --------------------------------------------------- cwd / user / stop

    #[test]
    fn working_dir_empty_defaults_root_and_relative_rejected() {
        // image has no WorkingDir -> "/"
        let s = convert(&spec(None, None), &ProcessOverrides::none()).unwrap();
        assert_eq!(s.cwd, "/");
        // image config with explicit "" WorkingDir -> still "/"
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux", "config": {"WorkingDir": ""}
        }))
        .unwrap();
        assert_eq!(img.working_dir, None);
        let s = convert(&img, &ProcessOverrides::none()).unwrap();
        assert_eq!(s.cwd, "/");
        // relative override rejected at the boundary
        let ov = ProcessOverrides {
            cwd: Some("relative/dir".into()),
            ..Default::default()
        };
        assert!(convert(&spec(None, None), &ov).is_err());
        let ov = ProcessOverrides {
            cwd: Some("/abs/dir".into()),
            ..Default::default()
        };
        assert_eq!(convert(&spec(None, None), &ov).unwrap().cwd, "/abs/dir");
    }

    #[test]
    fn user_empty_is_unset_and_never_folded_here() {
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux", "config": {"User": ""}
        }))
        .unwrap();
        assert_eq!(img.user, None);
        let s = convert(&img, &ProcessOverrides::none()).unwrap();
        assert_eq!(s.user, None); // folding to 0 is the resolver's disclosed act
        let s = convert(&img, &ProcessOverrides {
            user: Some("1000".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.user.as_deref(), Some("1000"));
        // image USER 1000 preserved verbatim through conversion
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux", "config": {"User": "1000"}
        }))
        .unwrap();
        let s = convert(&img, &ProcessOverrides::none()).unwrap();
        assert_eq!(s.user.as_deref(), Some("1000"));
    }

    #[test]
    fn stop_signal_passthrough_empty_is_unset() {
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux", "config": {"StopSignal": "SIGKILL"}
        }))
        .unwrap();
        let s = convert(&img, &ProcessOverrides::none()).unwrap();
        assert_eq!(s.stop_signal.as_deref(), Some("SIGKILL"));
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux", "config": {"StopSignal": ""}
        }))
        .unwrap();
        assert_eq!(img.stop_signal, None);
    }

    #[test]
    fn entrypoint_never_silently_replaced_with_tail() {
        // regression pinning of plans/tasks/T06.md review focus:
        // a config that declares a real entrypoint must surface it exactly.
        let img = ImageProcessConfig::from_config(&json!({
            "architecture": "amd64", "os": "linux",
            "config": {"Entrypoint": ["/real-entrypoint", "--strict"], "Cmd": ["arg"]}
        }))
        .unwrap();
        let s = convert(&img, &ProcessOverrides::none()).unwrap();
        assert_eq!(s.entrypoint, vec!["/real-entrypoint", "--strict"]);
        assert_eq!(s.argv, vec!["/real-entrypoint", "--strict", "arg"]);
        assert!(!s.argv.iter().any(|a| a == "tail" || a == "/dev/null"));
    }
}
