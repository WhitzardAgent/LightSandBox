#!/usr/bin/env python3
"""Run the pinned SWE-bench grader on a candidate execution log.

This process uses the worker-local Python environment and upstream checkout.
It never starts Docker; the input log must already have come from a real guest.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

UPSTREAM_REVISION = "02e7a74ffd0b707aab73d203fe87bdc7c76afc8e"
GRADING_SHA256 = "a095273010175001a6779e365b3b77abb8657c7ac2e4f80b4bc360faa6f09483"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--row", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    commit = subprocess.run(["git", "-C", str(args.upstream), "rev-parse", "HEAD"],
                            capture_output=True, text=True, check=True).stdout.strip()
    if commit != UPSTREAM_REVISION:
        raise RuntimeError(f"SWE-bench source revision mismatch: {commit}")
    if sha256(args.upstream / "swebench/harness/grading.py") != GRADING_SHA256:
        raise RuntimeError("SWE-bench grader bytes differ from pinned source")
    row = json.loads(args.row.read_text())
    if row.get("instance_id") != "astropy__astropy-11693":
        raise RuntimeError("SWE-bench row identity mismatch")
    content = args.log.read_text(errors="replace")
    if ">>>>> Start Test Output" not in content or ">>>>> End Test Output" not in content:
        raise RuntimeError("original verifier output markers absent")
    sys.path.insert(0, str(args.upstream))
    from swebench.harness.grading import get_eval_report, get_logs_eval
    from swebench.harness.utils import make_test_spec

    spec = make_test_spec(row)
    statuses, found = get_logs_eval(spec, str(args.log))
    prediction = {"instance_id": row["instance_id"], "model_patch": row["patch"],
                  "model_name_or_path": "gold"}
    report = get_eval_report(spec, prediction, str(args.log), include_tests_status=True)
    summary_lines = [line.strip() for line in content.splitlines()
                     if re.search(r"\bin\s+[\d.]+s\b", line)
                     and re.search(r"\b\d+\s+(?:passed|failed|skipped)\b", line)]
    raw_summary = summary_lines[-1] if summary_lines else None
    raw_counts = {kind: int(count) for count, kind in
                  re.findall(r"(\d+)\s+(failed|passed|skipped|errors|error|xfailed|xpassed)\b",
                             raw_summary or "")}
    outcome = {"schema": "swebench-candidate-grade/1", "upstream_revision": commit,
               "upstream_grading_sha256": sha256(args.upstream / "swebench/harness/grading.py"),
               "row_sha256": sha256(args.row), "log_sha256": sha256(args.log),
               "test_output_found": found, "parsed_test_count": len(statuses),
               "raw_summary": raw_summary, "raw_summary_counts": raw_counts,
               "raw_reported_total": sum(raw_counts.values()) if raw_counts else None,
               "status_map": statuses, "upstream_report": report}
    args.output.write_text(json.dumps(outcome, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"test_output_found": found, "parsed_test_count": len(statuses),
                      "raw_reported_total": outcome["raw_reported_total"],
                      "resolved": report[row["instance_id"]]["resolved"]}))
    return 0 if found and statuses else 3


if __name__ == "__main__":
    sys.exit(main())
