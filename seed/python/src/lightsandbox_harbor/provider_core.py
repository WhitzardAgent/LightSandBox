"""Concrete Harbor adapter logic with injected, operator-owned backend transport.

Dependency injection lets offline tests exercise forwarding without pretending to
run Harbor or Linux containers. The real wrapper in provider.py imports Harbor.
The backend transport/runtime is a subsequent implementation work package.
"""
from __future__ import annotations

import asyncio
from dataclasses import dataclass
import importlib
import os
from pathlib import Path
from typing import Protocol
from uuid import uuid4

from lightsandbox_seed.harbor_loader import inspect_environment
from .bridge import Command, ExecutionBackend, ExecutionBridge, Handle


@dataclass(frozen=True)
class StartRequest:
    environment_dir: Path
    environment_name: str
    session_id: str
    config: dict
    startup_env: dict[str, str]
    network_policy: dict
    phase_network_policies: tuple[dict, ...]
    cpu_policy: str
    memory_policy: str
    directory_targets: tuple[str, ...]
    force_build: bool


@dataclass(frozen=True)
class TransferRequest:
    direction: str
    kind: str
    local_path: Path
    guest_path: str

    def __post_init__(self):
        if self.direction not in ("upload", "download") or self.kind not in ("file", "directory"):
            raise ValueError("invalid transfer kind")
        if not self.guest_path.startswith("/") or "\0" in self.guest_path:
            raise ValueError("guest path must be an absolute path without NUL")
        # Guest symlink resolution belongs to the guest file helper. local_path
        # is a caller-side path; it MUST NOT be serialized as a daemon bind path.


class SandboxBackend(ExecutionBackend, Protocol):
    async def start(self, request: StartRequest) -> Handle: ...
    async def stop(self, handle: Handle, delete: bool) -> None: ...
    async def transfer(self, handle: Handle, request: TransferRequest) -> None: ...


def load_backend() -> SandboxBackend:
    spec = os.environ.get("LIGHTSANDBOX_BACKEND_FACTORY", "")
    if ":" not in spec:
        raise RuntimeError(
            "No LightSandbox runtime client is configured. This archive supplies "
            "a tested adapter seed, not a running sandbox daemon. Implement the "
            "native backend work package and set LIGHTSANDBOX_BACKEND_FACTORY="
            "trusted.module:create_backend. Do not select a factory from task metadata."
        )
    module, attribute = spec.split(":", 1)
    if not module or not attribute or attribute.startswith("_"):
        raise ValueError("invalid operator backend factory")
    factory = getattr(importlib.import_module(module), attribute)
    backend = factory()
    for method in ("start", "stop", "transfer", "events", "cancel"):
        if not callable(getattr(backend, method, None)):
            raise TypeError(f"backend missing required method: {method}")
    return backend


def _dump(value) -> dict:
    return value.model_dump(mode="json", exclude_none=True)


def make_environment_type(BaseEnvironment, EnvironmentCapabilities,
                          EnvironmentResourceCapabilities, HarborExecResult):
    class LightSandboxEnvironment(BaseEnvironment):
        """Single-service adapter seed; unsupported capabilities fail closed."""

        def __init__(self, *args, backend: SandboxBackend | None = None, **kwargs):
            self._backend = backend if backend is not None else load_backend()
            self._bridge = ExecutionBridge(self._backend)
            self._handle: Handle | None = None
            self._ready = False
            self._lifecycle_lock = asyncio.Lock()
            super().__init__(*args, **kwargs)

        @staticmethod
        def type() -> str:
            return "lightsandbox"

        @property
        def capabilities(self):
            # These are adapter protocol promises, NOT host probing results.
            # start() must refuse a host that cannot enforce its requested policy.
            return EnvironmentCapabilities(disable_internet=True, mounted=False,
                docker_compose=False, dynamic_network_policy=False)

        @classmethod
        def resource_capabilities(cls):
            return EnvironmentResourceCapabilities(cpu_limit=True, memory_limit=True)

        def _validate_definition(self):
            config = _dump(self.task_env_config)
            config = {k: v for k, v in config.items() if v is not None}
            inspection = inspect_environment(self.environment_dir, config)
            if inspection.kind == "compose":
                raise ValueError("Compose execution is not implemented by this adapter seed; complete T35/T36 before advertising it")
            if inspection.unknown_fields:
                raise ValueError(f"unreviewed Harbor environment fields: {inspection.unknown_fields}")

        def _running_handle(self) -> Handle:
            if self._handle is None or not self._ready:
                raise RuntimeError("environment is not READY")
            return self._handle

        async def start(self, force_build: bool) -> None:
            async with self._lifecycle_lock:
                if self._ready:
                    if force_build:
                        raise RuntimeError("force rebuild requires a new environment instance")
                    return
                if self._handle is not None:
                    raise RuntimeError("retained/stopped environment requires explicit reconciliation, not implicit recreation")
                request = StartRequest(
                    Path(self.environment_dir), self.environment_name, self.session_id,
                    _dump(self.task_env_config), self._startup_env(),
                    _dump(self.network_policy), tuple(_dump(p) for p in self._phase_network_policies),
                    self._cpu_resource_mode.value, self._memory_resource_mode.value,
                    tuple(m["target"] for m in self._mounts if m.get("target")), bool(force_build))
                # The backend must durably admit and wait for READY; a 202/build
                # operation reference is not a Handle and must not pass this check.
                handle = await self._backend.start(request)
                if not isinstance(handle, Handle):
                    raise TypeError("backend did not return a READY Handle")
                self._handle = handle
                self._ready = True
                try:
                    await self._upload_environment_dir_after_start()
                except BaseException as original:
                    self._ready = False
                    try:
                        await self._backend.stop(handle, True)
                    except BaseException as cleanup:
                        raise BaseExceptionGroup("startup and cleanup both failed", [original, cleanup])
                    self._handle = None
                    raise

        async def stop(self, delete: bool):
            async with self._lifecycle_lock:
                if self._handle is None:
                    return
                self._ready = False
                # Retain identity on failure. The operator must be able to retry
                # cleanup or reconcile it against the actual worker state.
                await self._backend.stop(self._handle, delete)
                if delete:
                    self._handle = None

        async def exec(self, command: str, cwd: str | None = None,
                       env: dict[str, str] | None = None, timeout_sec: int | None = None,
                       user: str | int | None = None):
            request = Command(uuid4().hex, command, cwd, self._merge_env(env) or {},
                              timeout_sec, self._resolve_user(user))
            result = await self._bridge.exec(self._running_handle(), request, self._output_callback())
            return HarborExecResult(stdout=result.stdout, stderr=result.stderr, return_code=result.return_code)

        async def upload_file(self, source_path: Path | str, target_path: str):
            await self._backend.transfer(self._running_handle(), TransferRequest("upload", "file", Path(source_path), target_path))

        async def upload_dir(self, source_dir: Path | str, target_dir: str):
            await self._backend.transfer(self._running_handle(), TransferRequest("upload", "directory", Path(source_dir), target_dir))

        async def download_file(self, source_path: str, target_path: Path | str):
            await self._backend.transfer(self._running_handle(), TransferRequest("download", "file", Path(target_path), source_path))

        async def download_dir(self, source_dir: str, target_dir: Path | str):
            await self._backend.transfer(self._running_handle(), TransferRequest("download", "directory", Path(target_dir), source_dir))

    return LightSandboxEnvironment
