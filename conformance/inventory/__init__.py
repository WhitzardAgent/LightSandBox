"""T02 benchmark inventory: enumeration, capability census and canary tools.

Stdlib-only. Deterministic: identical inputs always produce identical bytes
(that is the idempotence law checked by tests/inventory). Wall-clock
timestamps therefore never enter manifest or census bytes; fetch timestamps
live only in the lock file that is written once per real enumeration run.
"""
