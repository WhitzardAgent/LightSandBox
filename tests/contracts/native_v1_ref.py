"""T41 Python strict reference for native v1 contracts.

Mirrors the Rust contract types in crates/ls-core/src/contracts/ and the JSON
schemas in contracts/v1/. Any disagreement between the three engines over the
shared fixtures is a contract bug, by definition (T41-C01 parity rule).

Rules implemented here (frozen by fixtures, not by this file):
  * argv never word-split; empty trailing argument preserved; no NUL anywhere
  * JSON bool is never a number; integers are canonical (ms / bytes / millis)
  * null != missing: optional fields may be absent, explicit null is rejected
    (launch-mode entrypoint/command are the documented exceptions in the
    environment schema and are handled there, not here)
  * unknown fields are rejected, never silently ignored
"""
from __future__ import annotations

_NO_NUL = lambda s: isinstance(s, str) and "\0" not in s  # noqa: E731

_EXEC_TOP = {"command", "cwd", "env", "timeout_ms", "user"}


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


def validate_execspec(doc) -> dict:
    if not isinstance(doc, dict):
        return _err("execspec must be an object")
    unknown = set(doc) - _EXEC_TOP
    if unknown:
        return _err(f"unknown fields: {sorted(unknown)}")
    for key, value in doc.items():
        if value is None and key != "command":
            return _err(f"explicit null rejected for {key}")
    cmd = doc.get("command")
    if not isinstance(cmd, dict):
        return _err("command required")
    kind = cmd.get("kind")
    if kind == "argv":
        if set(cmd) != {"kind", "argv"}:
            return _err("argv form carries only kind+argv")
        argv = cmd["argv"]
        if not isinstance(argv, list) or not argv:
            return _err("argv must be a non-empty array")
        if not all(isinstance(a, str) and "\0" not in a for a in argv):
            return _err("argv entries must be NUL-free strings")
        if argv[0] == "":
            return _err("executable cannot be empty")
        normalized_command = {"kind": "argv", "argv": list(argv)}
    elif kind == "shell":
        if set(cmd) != {"kind", "executable", "script"}:
            return _err("shell form carries only kind+executable+script")
        exe, script = cmd["executable"], cmd["script"]
        if not (_NO_NUL(exe) and exe.startswith("/")):
            return _err("shell executable must be an absolute guest path")
        if not _NO_NUL(script):
            return _err("script must be a NUL-free string")
        normalized_command = {"kind": "shell", "executable": exe, "script": script}
    else:
        return _err("command.kind must be argv|shell")

    out = {"command": normalized_command, "env": {}}
    env = doc.get("env")
    if env is not None:
        if not isinstance(env, dict):
            return _err("env must be an object")
        for k, v in env.items():
            if not (_NO_NUL(k) and k and "=" not in k):
                return _err("invalid env name")
            if not _NO_NUL(v):
                return _err("invalid env value")
        out["env"] = dict(env)
    cwd = doc.get("cwd")
    if cwd is not None:
        if not (_NO_NUL(cwd) and cwd.startswith("/")):
            return _err("cwd must be an absolute guest path")
        out["cwd"] = cwd
    timeout = doc.get("timeout_ms")
    if timeout is not None:
        if isinstance(timeout, bool) or not isinstance(timeout, int) or timeout < 1:
            return _err("timeout_ms must be a positive integer (ms)")
        out["timeout_ms"] = timeout
    user = doc.get("user")
    if user is not None:
        if not (_NO_NUL(user) and user):
            return _err("user must be a non-empty NUL-free string")
        out["user"] = user
    return {"ok": True, "normalized": out}


_ENV_TOP = {"schema_version", "services", "networks", "volumes", "labels"}
_ENV_SVC = {"image", "launch_mode", "entrypoint", "command", "env", "cwd",
            "user", "resources", "mounts", "networks", "depends_on"}


def validate_environmentspec(doc) -> dict:
    """Strict offline subset: advanced collections must be present-empty;
    rejections name the offending field (never silently dropped)."""
    if not isinstance(doc, dict):
        return _err("environmentspec must be an object")
    unknown = set(doc) - _ENV_TOP
    if unknown:
        return _err(f"unknown fields: {sorted(unknown)}")
    services = doc.get("services")
    if not isinstance(services, dict) or not services:
        return _err("services must be a non-empty object")
    for top in ("networks", "volumes"):
        if doc.get(top):
            return _err(f"{top}: advanced field not implemented in offline subset")
    for name, svc in services.items():
        if not isinstance(svc, dict):
            return _err(f"services.{name} must be an object")
        bad = set(svc) - _ENV_SVC
        if bad:
            return _err(f"services.{name}: unknown fields: {sorted(bad)}")
        if not (_NO_NUL(svc.get("image")) and svc.get("image")):
            return _err(f"services.{name}: image required")
        for adv in ("mounts", "networks", "depends_on"):
            if svc.get(adv):
                return _err(f"services.{name}.{adv}: advanced field not implemented "
                            "in offline subset")
        mode = svc.get("launch_mode", "image")
        if mode not in ("image", "idle-agent"):
            return _err(f"services.{name}.launch_mode unknown: {mode!r}")
        for field in ("entrypoint", "command"):
            val = svc.get(field)
            if val is not None:
                if not isinstance(val, list) or not all(
                        isinstance(x, str) and "\0" not in x for x in val):
                    return _err(f"services.{name}.{field} must be an array of "
                                "NUL-free strings or null(inherit)")
                if val and val[0] == "":
                    return _err(f"services.{name}.{field}[0] cannot be empty")
        res = svc.get("resources")
        if res is not None:
            if not isinstance(res, dict):
                return _err(f"services.{name}.resources must be an object")
            for rk, rv in res.items():
                if rk not in ("cpu_millis", "memory_bytes", "storage_bytes", "pids"):
                    return _err(f"services.{name}.resources: unknown field {rk}")
                if isinstance(rv, bool) or not isinstance(rv, int) or rv < 1:
                    return _err(f"services.{name}.resources.{rk} must be a positive integer")
    return {"ok": True}


# ---------------------------------------------------------------- resolution

def normalize_spec(spec: dict) -> dict:
    out = dict(spec)
    out.setdefault("requires", {})
    for key in ("multi_uid", "hard_memory_limit", "private_proc", "seccomp",
                "outbound_network"):
        out["requires"].setdefault(key, False)
    out["single_id_compatibility_policy"] = spec.get(
        "single_id_compatibility_policy", "STRICT")
    return out


def _dig(host: dict, dotted: str):
    cur = host
    for part in dotted.split("."):
        cur = cur[part]
    return cur


def resolve(host: dict, spec: dict, rules: dict) -> dict:
    norm = normalize_spec(spec)
    rejections: list[dict] = []
    degradations: list[dict] = []
    for req, mapping in rules["requirements_map"].items():
        if not norm["requires"].get(req):
            continue
        actual = _dig(host, mapping["capability"])
        if actual != mapping["accepted_value"]:
            rejections.append({
                "requirement": req,
                "code": rules["rejection"]["code"],
                "capability": mapping["capability"],
            })
    user = spec.get("image", {}).get("user")
    if user and user not in ("0", "root") and \
            host["identity"]["uid_mapping_mode"] == "SINGLE_ID":
        if norm["single_id_compatibility_policy"] == "FLATTEN_ROOT":
            degradations.append({
                "capability": "identity.ownership_model",
                "mode": "FLATTEN_ROOT",
                "detail": f"image user {user} folded to uid 0",
            })
        else:
            rejections.append({
                "requirement": "image.user",
                "code": rules["policy_rejection"]["code"],
                "capability": "identity.image_user_support",
            })
    effective = {dim: host[dim] for dim in
                 ("identity", "process", "resource", "filesystem", "network", "runtime")}
    return {
        "outcome": "REJECT" if rejections else "ACCEPT",
        "rejections": rejections,
        "degradations": degradations,
        "effective": effective,
        "tier": host.get("tier"),
    }


# ---------------------------------------------------------------- states

def legal_transition(states: dict, domain: str, frm: str, to: str) -> bool:
    return to in states["domains"][domain].get(frm, [])


def check_semantic_fact(doc: dict) -> bool:
    execution = doc.get("execution")
    if execution and execution.get("state") == "exited" and not execution.get("exit_record"):
        return False
    return True


# ---------------------------------------------------------------- identity

class ContractRejection(Exception):
    def __init__(self, code: str, http_status: int):
        super().__init__(code)
        self.code = code
        self.http_status = http_status


class SandboxIdentity:
    """generation = incarnation fence; revision = record CAS. Values are
    persisted; an API/manager restart reloads them unchanged."""

    def __init__(self, sandbox_id: str, generation: int, revision: int):
        self.sandbox_id = sandbox_id
        self.generation = generation
        self.revision = revision

    def api_restart(self):
        return self  # process restart never mints a new incarnation

    def recreate_incarnation(self):
        return SandboxIdentity(self.sandbox_id, self.generation + 1, 0)

    def cas_update(self, expected_revision: int):
        if expected_revision != self.revision:
            raise ContractRejection("STALE_GENERATION", 409)
        self.revision += 1
        return self.revision

    def bind_execution(self, execution_id: str, claimed_generation: int):
        if claimed_generation != self.generation:
            raise ContractRejection("STALE_GENERATION", 409)
        return {"execution_id": execution_id, "generation": self.generation}


def apply_action(ident: SandboxIdentity, action: dict) -> dict:
    kind = action["kind"]
    if kind == "api_restart":
        after = ident.api_restart()
        return {"generation": after.generation, "revision": after.revision}
    if kind == "recreate_incarnation":
        after = ident.recreate_incarnation()
        return {"generation": after.generation, "revision": after.revision}
    if kind == "cas_update":
        try:
            rev = ident.cas_update(action["expected_revision"])
            return {"outcome": "ACCEPT", "revision": rev,
                    "generation": ident.generation}
        except ContractRejection as exc:
            return {"outcome": "REJECT",
                    "error": {"code": exc.code, "http_status": exc.http_status},
                    "revision": ident.revision, "generation": ident.generation}
    if kind == "bind_execution":
        try:
            ident.bind_execution(action["execution_id"], action["claimed_generation"])
            return {"outcome": "ACCEPT"}
        except ContractRejection as exc:
            return {"outcome": "REJECT",
                    "error": {"code": exc.code, "http_status": exc.http_status}}
    raise ValueError(f"unknown action {kind!r}")
