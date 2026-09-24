"""Harbor-facing execution semantics over a runtime-neutral backend protocol.

No host commands are executed here. Backend implementers must run command strings
inside the guest, preserve the execution_id and generation, and produce an
explicit final receipt. Events use our development contract, not OpenSandbox SSE.
"""
from __future__ import annotations

import asyncio
import base64
import codecs
from contextlib import suppress
from dataclasses import dataclass, field
import math
from types import MappingProxyType
from typing import AsyncIterator, Awaitable, Callable, Mapping, Protocol

from lightsandbox_seed.events import ExecAccumulator


class BackendError(RuntimeError):
    """Infrastructure/transport failure; never interpreted as command success."""


@dataclass(frozen=True)
class Handle:
    sandbox_id: str
    generation: int

    def __post_init__(self):
        if not isinstance(self.sandbox_id, str) or not self.sandbox_id or type(self.generation) is not int or self.generation < 1:
            raise ValueError("invalid sandbox identity/generation")


@dataclass(frozen=True)
class Command:
    execution_id: str
    command: str
    cwd: str | None = None
    env: Mapping[str, str] = field(default_factory=dict)
    timeout_sec: float | None = None
    user: str | int | None = None

    def __post_init__(self):
        if not self.execution_id or not isinstance(self.command, str) or "\0" in self.command:
            raise ValueError("invalid command")
        if self.timeout_sec is not None and (type(self.timeout_sec) not in (int, float) or not math.isfinite(self.timeout_sec) or self.timeout_sec <= 0):
            raise ValueError("timeout must be positive and finite")
        if any(not isinstance(k, str) or not isinstance(v, str) or "\0" in k + v or "=" in k for k, v in self.env.items()):
            raise ValueError("invalid environment mapping")
        object.__setattr__(self, "env", MappingProxyType(dict(self.env)))


@dataclass(frozen=True)
class TextResult:
    stdout: str
    stderr: str
    return_code: int


class ExecutionBackend(Protocol):
    def events(self, handle: Handle, request: Command) -> AsyncIterator[dict]: ...
    async def cancel(self, handle: Handle, execution_id: str) -> None: ...


class ExecutionBridge:
    def __init__(self, backend: ExecutionBackend):
        self.backend = backend

    async def exec(self, handle: Handle, request: Command,
                   callback: Callable[[str, str], Awaitable[None]] | None = None) -> TextResult:
        accumulator = ExecAccumulator(request.execution_id, handle.generation)
        decoders = {s: codecs.getincrementaldecoder("utf-8")("replace") for s in ("stdout", "stderr")}
        seen: set[int] = set()

        async def collect() -> TextResult:
            async for event in self.backend.events(handle, request):
                accumulator.accept(event)
                seq = event["sequence"]
                if seq in seen:
                    continue
                seen.add(seq)
                if event["kind"] == "output" and callback is not None:
                    text = decoders[event["stream"]].decode(base64.b64decode(event["data"], validate=True))
                    if text:
                        await callback(text, event["stream"])
            result = accumulator.result()
            if callback is not None:
                for stream, decoder in decoders.items():
                    tail = decoder.decode(b"", final=True)
                    if tail:
                        await callback(tail, stream)
            return TextResult(result.stdout.decode("utf-8", "replace"),
                              result.stderr.decode("utf-8", "replace"), result.return_code)

        try:
            if request.timeout_sec is None:
                return await collect()
            async with asyncio.timeout(request.timeout_sec):
                return await collect()
        except (asyncio.CancelledError, TimeoutError):
            # Bounded best effort. A cancellation acknowledgement is NOT proof
            # that the process has exited; the server must reconcile it later.
            with suppress(Exception):
                await asyncio.wait_for(self.backend.cancel(handle, request.execution_id), timeout=2.0)
            raise
        except ValueError as exc:
            raise BackendError(f"invalid or incomplete execution stream: {exc}") from exc
