//! Unprivileged development worker: one OCI layout, one native sandbox,
//! ordered argv executions, authoritative cleanup. Benchmark interpretation
//! stays in dev/runtime_cases.py and the upstream harness.

use ls_core::domain::execution::ExitRecord;
use ls_core::domain::ids::OwnerId;
use ls_host::images::process_config::ArgvOverride;
use ls_host::images::store::ImageStore;
use ls_host::images::Platform;
use ls_host::runtime::backend::SandboxBackend;
use ls_host::runtime::manager::{CreateSandboxInput, ExecRequest, ManagerConfig, SandboxManager};
use ls_host::runtime::native::runc::RuncRuntime;
use ls_host::runtime::native::{NativeBackendConfig, NativeRuncBackend};
use serde::Deserialize;
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
struct Request {
    layout: PathBuf,
    workspace: PathBuf,
    runc: PathBuf,
    umoci: PathBuf,
    guest_init: PathBuf,
    #[serde(default)]
    entrypoint_override: Option<Vec<String>>,
    #[serde(default)]
    command_override: Option<Vec<String>>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
    #[serde(default)]
    steps: Vec<Step>,
    #[serde(default)]
    asset_root: Option<PathBuf>,
    #[serde(default)]
    file_imports: Vec<FileImport>,
    #[serde(default)]
    file_exports: Vec<FileExport>,
}

#[derive(Deserialize)]
struct FileImport {
    source: PathBuf,
    target: String,
    sha256: String,
}

#[derive(Deserialize)]
struct FileExport {
    source: String,
    name: String,
    max_bytes: u64,
}

#[derive(Deserialize)]
struct Step {
    name: String,
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    require_exit_zero: bool,
}

fn self_identity() -> (u32, u32) {
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn getegid() -> u32;
    }
    unsafe { (geteuid(), getegid()) }
}

fn override_arg(value: Option<Vec<String>>) -> Option<ArgvOverride> {
    value.map(|v| {
        if v.is_empty() {
            ArgvOverride::Clear
        } else {
            ArgvOverride::Replace(v)
        }
    })
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn decode_base64(input: &str) -> AnyResult<Vec<u8>> {
    let data = input.trim().as_bytes();
    if data.len() % 4 != 0 {
        return Err("guest base64 output has invalid length".into());
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    for (i, chunk) in data.chunks(4).enumerate() {
        let value = |b: u8| -> AnyResult<u8> {
            match b {
                b'A'..=b'Z' => Ok(b - b'A'),
                b'a'..=b'z' => Ok(b - b'a' + 26),
                b'0'..=b'9' => Ok(b - b'0' + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                _ => Err("guest base64 output contains non-alphabet byte".into()),
            }
        };
        if chunk[0] == b'=' || chunk[1] == b'=' {
            return Err("guest base64 output has invalid padding".into());
        }
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            let c = value(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] != b'=' {
                let d = value(chunk[3])?;
                out.push((c << 6) | d);
            }
        } else if chunk[3] != b'=' {
            return Err("guest base64 output has invalid padding".into());
        }
        if (chunk[2] == b'=' || chunk[3] == b'=') && i + 1 != data.len() / 4 {
            return Err("guest base64 output pads before final block".into());
        }
    }
    Ok(out)
}

fn validate_request(r: &Request) -> AnyResult<()> {
    for (name, path) in [
        ("layout", &r.layout),
        ("workspace", &r.workspace),
        ("runc", &r.runc),
        ("umoci", &r.umoci),
        ("guest_init", &r.guest_init),
    ] {
        if !path.is_absolute() {
            return Err(format!("{name} must be an absolute path").into());
        }
    }
    if !r.layout.is_dir() || !r.runc.is_file() || !r.umoci.is_file() || !r.guest_init.is_file() {
        return Err("layout, runc, umoci and guest_init must exist".into());
    }
    if r.steps
        .iter()
        .any(|s| s.argv.is_empty() || s.name.is_empty())
    {
        return Err("every step needs a name and nonempty argv".into());
    }
    if !r.file_imports.is_empty() && r.asset_root.is_none() {
        return Err("file imports require an absolute asset_root".into());
    }
    if let Some(asset_root) = &r.asset_root {
        if !asset_root.is_absolute() || !asset_root.is_dir() {
            return Err("asset_root must be an absolute directory".into());
        }
        let root = fs::canonicalize(asset_root)?;
        for file in &r.file_imports {
            let source = fs::canonicalize(&file.source)?;
            if !source.starts_with(&root) || !source.is_file() {
                return Err(format!("file import escapes asset root: {}", source.display()).into());
            }
            let path = Path::new(&file.target);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || !file
                    .target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                || file.target == "/"
            {
                return Err(format!("unsafe guest file target: {}", file.target).into());
            }
            if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("file import sha256 must be 64 hex characters".into());
            }
        }
    }
    for file in &r.file_exports {
        let path = Path::new(&file.source);
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || !file
                .source
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
            || file.name.is_empty()
            || !file
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || file.max_bytes == 0
        {
            return Err("invalid file export source, name, or byte limit".into());
        }
    }
    Ok(())
}

fn run(r: &Request) -> AnyResult<Value> {
    validate_request(r)?;
    fs::create_dir_all(&r.workspace)?;
    let image_root = r.workspace.join("images");
    let host = Platform::host();
    let store = ImageStore::open(&image_root)?;
    let imported = store.import(
        &r.layout,
        Some("runtime"),
        &host,
        "candidate-case-layout",
        "candidate-case",
    )?;
    let image_digest = imported.image.digest.as_str().to_string();
    let image_hex = image_digest
        .strip_prefix("sha256:")
        .ok_or("bad image digest")?
        .to_string();
    let identity = self_identity();
    if identity.0 == 0 {
        return Err("case driver requires a non-root host user".into());
    }
    let runtime = RuncRuntime::new(
        &r.runc,
        r.workspace.join("runc"),
        Some(identity),
        r.workspace.join("home"),
    );
    fs::create_dir_all(r.workspace.join("home"))?;
    let backend = NativeRuncBackend::new(NativeBackendConfig {
        runc: runtime,
        guest_init: r.guest_init.clone(),
        native_root: r.workspace.join("native"),
        identity,
    })?;
    let cfg = ManagerConfig {
        state_root: r.workspace.join("state"),
        images_root: image_root,
        rootfs_root: r.workspace.join("rootfs"),
        umoci_binary: r.umoci.clone(),
        host,
        owner: OwnerId::new("candidate-case")?,
        runtime_identity: Some(identity),
    };
    let (mut manager, open) = SandboxManager::open(cfg, backend)?;
    if !open.foreign_instances.is_empty() {
        return Err(format!("foreign live instances: {:?}", open.foreign_instances).into());
    }
    let input = CreateSandboxInput {
        image: image_hex,
        workdir: r.workdir.clone(),
        env: r.env.clone(),
        entrypoint_override: override_arg(r.entrypoint_override.clone()),
        command_override: override_arg(r.command_override.clone()),
        requirements: Default::default(),
        preference: Default::default(),
    };
    let created = manager.create(&input)?;
    let log_dir = r.workspace.join("logs");
    let mut steps = Vec::new();
    let mut file_imports = Vec::new();
    let mut file_exports = Vec::new();
    let mut step_error: Option<String> = None;
    if let Err(e) = fs::create_dir_all(&log_dir) {
        step_error = Some(format!("log directory: {e}"));
    }
    for file in &r.file_imports {
        if step_error.is_some() {
            break;
        }
        let data = match fs::read(&file.source) {
            Ok(data) => data,
            Err(e) => {
                step_error = Some(format!("asset read {}: {e}", file.source.display()));
                break;
            }
        };
        let host_sha = ls_core::domain::idempotency::sha256_hex(&data);
        if host_sha != file.sha256 {
            step_error = Some(format!("asset digest changed: {}", file.source.display()));
            break;
        }
        let parent = Path::new(&file.target)
            .parent()
            .unwrap()
            .display()
            .to_string();
        let mkdir = manager.execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["mkdir".into(), "-p".into(), parent],
                env: Vec::new(),
                cwd: None,
                timeout: Some(Duration::from_secs(20)),
            },
        );
        match mkdir {
            Ok(outcome)
                if matches!(
                    outcome.exit_record,
                    Some(ExitRecord::Exited { exit_code: 0 })
                ) => {}
            Ok(outcome) => {
                step_error = Some(format!(
                    "file import mkdir {}: {:?}",
                    file.target, outcome.exit_record
                ));
                break;
            }
            Err(e) => {
                step_error = Some(format!("file import mkdir {}: {e}", file.target));
                break;
            }
        }
        let mut chunk_count = 0usize;
        let mut chunk_receipts = Vec::new();
        for (i, chunk) in data.chunks(48 * 1024).enumerate() {
            let encoded = base64(chunk);
            let redirect = if i == 0 { ">" } else { ">>" };
            let command = format!(
                "printf %s '{encoded}' | base64 -d {redirect} '{}'",
                file.target
            );
            let outcome = manager.execute(
                &created.sandbox_id,
                created.generation,
                &ExecRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), command],
                    env: Vec::new(),
                    cwd: None,
                    timeout: Some(Duration::from_secs(20)),
                },
            );
            match outcome {
                Ok(o)
                    if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 }))
                        && !o.truncated =>
                {
                    chunk_count += 1;
                    chunk_receipts.push(json!({"index": i, "bytes": chunk.len(),
                        "sha256": ls_core::domain::idempotency::sha256_hex(chunk),
                        "execution_id": o.execution_id.as_str(), "exit_code": 0}));
                }
                Ok(o) => {
                    let stderr = log_dir.join(format!("import-{i}.stderr"));
                    let _ = fs::write(&stderr, o.stderr.as_bytes());
                    step_error = Some(format!(
                        "file import {} chunk {i}: {:?}; stderr={}",
                        file.target,
                        o.exit_record,
                        stderr.display()
                    ));
                    break;
                }
                Err(e) => {
                    step_error = Some(format!("file import {} chunk {i}: {e}", file.target));
                    break;
                }
            }
        }
        if step_error.is_some() {
            file_imports.push(json!({"source": file.source, "target": file.target,
                "host_sha256": host_sha, "bytes": data.len(), "chunks_completed": chunk_count,
                "chunk_receipts": chunk_receipts, "complete": false}));
            break;
        }
        if data.is_empty() {
            let outcome = manager.execute(
                &created.sandbox_id,
                created.generation,
                &ExecRequest {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(": > '{}'", file.target),
                    ],
                    env: Vec::new(),
                    cwd: None,
                    timeout: Some(Duration::from_secs(20)),
                },
            );
            if !matches!(outcome, Ok(ref o) if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 })))
            {
                step_error = Some(format!("empty file import failed: {}", file.target));
                break;
            }
        }
        let checked = manager.execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["sha256sum".into(), file.target.clone()],
                env: Vec::new(),
                cwd: None,
                timeout: Some(Duration::from_secs(20)),
            },
        );
        let guest_sha = match checked {
            Ok(o)
                if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 }))
                    && !o.truncated =>
            {
                o.stdout.split_whitespace().next().unwrap_or("").to_string()
            }
            Ok(o) => {
                step_error = Some(format!(
                    "file import digest {}: {:?}",
                    file.target, o.exit_record
                ));
                String::new()
            }
            Err(e) => {
                step_error = Some(format!("file import digest {}: {e}", file.target));
                String::new()
            }
        };
        if step_error.is_none() && guest_sha != host_sha {
            step_error = Some(format!("file import digest mismatch: {}", file.target));
        }
        file_imports.push(json!({"source": file.source, "target": file.target,
            "host_sha256": host_sha, "guest_sha256": guest_sha,
            "bytes": data.len(), "chunks_completed": chunk_count,
            "chunk_receipts": chunk_receipts, "complete": step_error.is_none()}));
    }
    for (i, step) in r.steps.iter().enumerate() {
        if step_error.is_some() {
            break;
        }
        let outcome = match manager.execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: step.argv.clone(),
                env: step.env.clone(),
                cwd: step.cwd.clone(),
                timeout: step.timeout_ms.map(Duration::from_millis),
            },
        ) {
            Ok(value) => value,
            Err(e) => {
                step_error = Some(format!("step {}: {e}", step.name));
                break;
            }
        };
        let stdout_path = log_dir.join(format!("{i}.stdout"));
        let stderr_path = log_dir.join(format!("{i}.stderr"));
        if let Err(e) = fs::write(&stdout_path, outcome.stdout.as_bytes()) {
            step_error = Some(format!("step {} stdout write: {e}", step.name));
            break;
        }
        if let Err(e) = fs::write(&stderr_path, outcome.stderr.as_bytes()) {
            step_error = Some(format!("step {} stderr write: {e}", step.name));
            break;
        }
        let exit_record = match outcome.exit_record {
            Some(ExitRecord::Exited { exit_code }) => {
                json!({"kind":"EXITED", "exit_code":exit_code})
            }
            Some(ExitRecord::Signaled { signal }) => json!({"kind":"SIGNALED", "signal":signal}),
            Some(ExitRecord::Unknown) => json!({"kind":"UNKNOWN"}),
            None => Value::Null,
        };
        let exited_zero = exit_record.get("exit_code").and_then(Value::as_i64) == Some(0);
        steps.push(json!({
            "name": step.name, "argv": step.argv, "cwd": step.cwd,
            "execution_id": outcome.execution_id.as_str(),
            "state": outcome.state.wire(), "exit_record": exit_record,
            "wall_ms": outcome.wall_ms, "truncated": outcome.truncated,
            "stdout": stdout_path, "stderr": stderr_path,
            "stdout_sha256": ls_core::domain::idempotency::sha256_hex(outcome.stdout.as_bytes()),
            "stderr_sha256": ls_core::domain::idempotency::sha256_hex(outcome.stderr.as_bytes())
        }));
        if step.require_exit_zero && !exited_zero {
            step_error = Some(format!(
                "step {} did not exit zero with an authoritative receipt",
                step.name
            ));
            break;
        }
        if outcome.truncated {
            step_error = Some(format!("step {} output was truncated", step.name));
            break;
        }
    }
    for file in &r.file_exports {
        if step_error.is_some() {
            break;
        }
        let digest = manager.execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["sha256sum".into(), file.source.clone()],
                env: Vec::new(),
                cwd: None,
                timeout: Some(Duration::from_secs(20)),
            },
        );
        let guest_sha = match digest {
            Ok(o)
                if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 }))
                    && !o.truncated =>
            {
                o.stdout.split_whitespace().next().unwrap_or("").to_string()
            }
            Ok(o) => {
                step_error = Some(format!(
                    "file export digest {}: {:?}",
                    file.source, o.exit_record
                ));
                break;
            }
            Err(e) => {
                step_error = Some(format!("file export digest {}: {e}", file.source));
                break;
            }
        };
        let size_check = manager.execute(
            &created.sandbox_id,
            created.generation,
            &ExecRequest {
                argv: vec!["stat".into(), "-c".into(), "%s".into(), file.source.clone()],
                env: Vec::new(),
                cwd: None,
                timeout: Some(Duration::from_secs(20)),
            },
        );
        let size = match size_check {
            Ok(o)
                if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 }))
                    && !o.truncated =>
            {
                match o.stdout.trim().parse::<u64>() {
                    Ok(size) => size,
                    Err(e) => {
                        step_error = Some(format!("file export size {}: {e}", file.source));
                        break;
                    }
                }
            }
            Ok(o) => {
                step_error = Some(format!(
                    "file export stat {}: {:?}",
                    file.source, o.exit_record
                ));
                break;
            }
            Err(e) => {
                step_error = Some(format!("file export stat {}: {e}", file.source));
                break;
            }
        };
        if size > file.max_bytes {
            step_error = Some(format!(
                "file export {} exceeds {} bytes",
                file.source, file.max_bytes
            ));
            break;
        }
        let export_dir = r.workspace.join("exports");
        if let Err(e) = fs::create_dir_all(&export_dir) {
            step_error = Some(format!(
                "file export directory {}: {e}",
                export_dir.display()
            ));
            break;
        }
        let dest = export_dir.join(&file.name);
        let mut out = match fs::File::create(&dest) {
            Ok(out) => out,
            Err(e) => {
                step_error = Some(format!("file export destination {}: {e}", dest.display()));
                break;
            }
        };
        let count = size.div_ceil(32 * 1024);
        let mut chunks_completed = 0;
        for i in 0..count {
            let command = format!(
                "dd if='{}' bs=32768 skip={i} count=1 status=none | base64 -w0",
                file.source
            );
            let chunk = manager.execute(
                &created.sandbox_id,
                created.generation,
                &ExecRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), command],
                    env: Vec::new(),
                    cwd: None,
                    timeout: Some(Duration::from_secs(20)),
                },
            );
            match chunk {
                Ok(o)
                    if matches!(o.exit_record, Some(ExitRecord::Exited { exit_code: 0 }))
                        && !o.truncated =>
                {
                    match decode_base64(&o.stdout) {
                        Ok(bytes) => {
                            if let Err(e) = out.write_all(&bytes) {
                                step_error = Some(format!(
                                    "file export {} chunk {i} write: {e}",
                                    file.source
                                ));
                                break;
                            }
                            chunks_completed += 1;
                        }
                        Err(e) => {
                            step_error =
                                Some(format!("file export {} chunk {i}: {e}", file.source));
                            break;
                        }
                    }
                }
                Ok(o) => {
                    step_error = Some(format!(
                        "file export {} chunk {i}: {:?}",
                        file.source, o.exit_record
                    ));
                    break;
                }
                Err(e) => {
                    step_error = Some(format!("file export {} chunk {i}: {e}", file.source));
                    break;
                }
            }
        }
        if let Err(e) = out.flush() {
            step_error = Some(format!("file export {} flush: {e}", file.source));
        }
        let host_bytes = match fs::read(&dest) {
            Ok(bytes) => bytes,
            Err(e) => {
                step_error = Some(format!("file export {} readback: {e}", file.source));
                Vec::new()
            }
        };
        let host_sha = ls_core::domain::idempotency::sha256_hex(&host_bytes);
        if step_error.is_none() && (host_bytes.len() as u64 != size || host_sha != guest_sha) {
            step_error = Some(format!(
                "file export digest or size mismatch: {}",
                file.source
            ));
        }
        file_exports.push(json!({"source": file.source, "path": dest,
            "bytes": host_bytes.len(), "guest_sha256": guest_sha,
            "host_sha256": host_sha, "chunks_completed": chunks_completed,
            "complete": step_error.is_none()}));
    }
    let cleanup = manager.delete(&created.sandbox_id, created.generation);
    let live_after = manager.backend().enumerate_live();
    let cleanup_value = match cleanup {
        Ok(info) => json!({"all_ok": info.all_ok(), "receipt": info}),
        Err(e) => json!({"all_ok": false, "error": e.to_string()}),
    };
    let complete = step_error.is_none() && cleanup_value["all_ok"] == true && live_after.is_empty();
    Ok(json!({
        "schema": "native-case-driver/1", "image_digest": image_digest,
        "sandbox_id": created.sandbox_id.as_str(),
        "generation": created.generation.get(),
        "created_state": created.observed.wire(),
        "steps": steps, "file_imports": file_imports, "file_exports": file_exports,
        "step_error": step_error,
        "cleanup": cleanup_value, "live_after": live_after,
        "complete": complete,
    }))
}

fn write_receipt(path: &Path, value: &Value) -> AnyResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() != 3 {
        eprintln!("usage: ls-case-driver REQUEST.json RECEIPT.json");
        std::process::exit(2);
    }
    let receipt_path = PathBuf::from(&argv[2]);
    let result = (|| -> AnyResult<Value> {
        let req: Request = serde_json::from_slice(&fs::read(&argv[1])?)?;
        run(&req)
    })();
    let value = match result {
        Ok(value) => value,
        Err(error) => json!({"schema": "native-case-driver/1", "complete": false,
                             "error": error.to_string()}),
    };
    if let Err(error) = write_receipt(&receipt_path, &value) {
        eprintln!("receipt write failed: {error}");
        std::process::exit(1);
    }
    println!("{}", receipt_path.display());
    std::process::exit(if value["complete"] == true { 0 } else { 1 });
}
