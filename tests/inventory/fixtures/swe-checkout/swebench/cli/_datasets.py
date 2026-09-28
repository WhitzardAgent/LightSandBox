"""Fixture copy of the locked SWE-bench alias map shape (rev 02e7a74)."""

DATASET_ALIASES = {
    "full": "SWE-bench/SWE-bench",
    "lite": "SWE-bench/SWE-bench_Lite",
    "verified": "SWE-bench/SWE-bench_Verified",
}


def resolve_dataset(name: str) -> str:
    return DATASET_ALIASES.get(name.strip().lower(), name)
