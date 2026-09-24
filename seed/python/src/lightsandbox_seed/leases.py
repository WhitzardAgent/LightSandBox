"""Thread-safe admission/fencing reference. NOT a durable scheduler or cgroup driver.

The production Rust service must transact this state with its operation journal.
Reservations stay charged during DRAINING; an independently obtained cleanup
receipt is required before capacity becomes reusable. We deliberately create a
fresh sandbox identity for every episode instead of pretending workspace deletion
is a full environment reset.
"""
from __future__ import annotations

from dataclasses import dataclass
from threading import RLock
from uuid import uuid4
import re


class LeaseError(RuntimeError):
    pass


@dataclass(frozen=True)
class Capacity:
    cpu_millis: int
    memory_mb: int
    slots: int

    def __post_init__(self):
        if any(type(v) is not int or v <= 0 for v in (self.cpu_millis, self.memory_mb, self.slots)):
            raise ValueError("resource quantities must be positive integers")


@dataclass(frozen=True)
class Lease:
    lease_id: str
    generation: int
    owner: str
    request_key: str
    capacity: Capacity
    state: str = "ACTIVE"
    cleanup_receipt: str | None = None


class LeasePool:
    def __init__(self, capacity: Capacity):
        self.capacity = capacity
        self._lock = RLock()
        self._leases: dict[str, Lease] = {}
        self._requests: dict[tuple[str, str], str] = {}

    def acquire(self, owner: str, request_key: str, amount: Capacity) -> Lease:
        if not owner or not request_key:
            raise ValueError("owner and request key required")
        with self._lock:
            key = (owner, request_key)
            if key in self._requests:
                old = self._leases[self._requests[key]]
                if old.capacity != amount or old.state != "ACTIVE":
                    raise LeaseError("request conflicts with an existing or retired lease")
                return old
            charged = [x.capacity for x in self._leases.values() if x.state != "RELEASED"]
            for dimension in ("cpu_millis", "memory_mb", "slots"):
                if sum(getattr(x, dimension) for x in charged) + getattr(amount, dimension) > getattr(self.capacity, dimension):
                    raise LeaseError(f"capacity exhausted: {dimension}")
            lease = Lease(uuid4().hex, 1, owner, request_key, amount)
            self._leases[lease.lease_id] = lease
            self._requests[key] = lease.lease_id
            return lease

    def _lookup(self, owner: str, lease_id: str, generation: int) -> Lease:
        lease = self._leases.get(lease_id)
        if lease is None or lease.owner != owner or type(generation) is not int or lease.generation != generation:
            raise LeaseError("unknown, foreign, or stale lease")
        return lease

    def check(self, owner: str, lease_id: str, generation: int) -> Lease:
        with self._lock:
            lease = self._lookup(owner, lease_id, generation)
            if lease.state != "ACTIVE":
                raise LeaseError("lease is not accepting operations")
            return lease

    def begin_release(self, owner: str, lease_id: str, generation: int) -> Lease:
        with self._lock:
            lease = self._lookup(owner, lease_id, generation)
            if lease.state == "RELEASED":
                return lease
            next_lease = Lease(lease.lease_id, lease.generation, lease.owner, lease.request_key,
                               lease.capacity, "DRAINING")
            self._leases[lease_id] = next_lease
            return next_lease

    def confirm_clean(self, owner: str, lease_id: str, generation: int, receipt: str) -> Lease:
        if not isinstance(receipt, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", receipt):
            raise LeaseError("cleanup receipt hash required")
        with self._lock:
            lease = self._lookup(owner, lease_id, generation)
            if lease.state != "DRAINING":
                raise LeaseError("cleanup completion requires DRAINING state")
            result = Lease(lease.lease_id, lease.generation, lease.owner, lease.request_key,
                           lease.capacity, "RELEASED", receipt)
            self._leases[lease_id] = result
            return result
