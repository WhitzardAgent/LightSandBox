"""Candidate case runner failure controls; these do not claim isolation."""
import json
import socket
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import subprocess

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "dev"))
import runtime_cases  # noqa: E402
import checklib  # noqa: E402


class RuntimeCaseControls(unittest.TestCase):
    def test_original_swe_eval_script_is_required(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "swebench/full/astropy__astropy-11693.json"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"instance_id": "astropy__astropy-11693",
                                        "patch": "real patch", "eval_script": ""}))
            _, _, reason = runtime_cases.inspect_assets(
                "swebench:full:astropy__astropy-11693", Path(tmp))
            self.assertIn("eval script", reason)

    def test_swe_script_records_test_exit_before_end_marker(self):
        raw = '#!/bin/bash\nset -uxo pipefail\necho ">>>>> Start Test Output"\nfalse\necho ">>>>> End Test Output"\n'
        script = runtime_cases.swe_harness_eval_script(raw)
        self.assertIn('false\nSWEBENCH_TEST_EXIT_CODE=$?\necho ">>>>> End Test Output"', script)
        self.assertIn('echo ">>>>> Test Exit Code: $SWEBENCH_TEST_EXIT_CODE"', script)

    def test_arvo_missing_assets_cannot_reach_image_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cybergym/arvo-47101.json"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"task_id": "arvo:47101"}))
            metadata, _, reason = runtime_cases.inspect_assets(
                "cybergym:main:arvo:47101", Path(tmp))
            self.assertEqual(len(metadata["missing_assets"]), 5)
            self.assertIn("repo-vul.tar.gz", reason)

    def test_child_exit_17_and_raw_log_are_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            rec = runtime_cases.run_command(
                [sys.executable, "-c", "import sys; print('ran'); sys.exit(17)"],
                root, root, "exit17")
            self.assertEqual(rec["exit_code"], 17)
            self.assertEqual(rec["termination"], "EXITED")
            self.assertEqual((root / "exit17.stdout").read_text(), "ran\n")
            self.assertEqual(rec["stdout_sha256"], runtime_cases.sha256(root / "exit17.stdout"))

    def test_unregistered_host_is_blocked_before_effects(self):
        code, result = runtime_cases.run(
            "swebench:full:astropy__astropy-11693",
            {"authorized": True, "hostname": socket.gethostname() + "-wrong",
             "uid": 0, "workspace_root": "/nonexistent"})
        self.assertEqual(code, 3)
        self.assertIn("identity", result["error"])

    def test_budget_sample_survives_disappearing_build_temp_file(self):
        raced = subprocess.CompletedProcess(["du"], 1, "8200\t/work\n",
                                         "du: cannot access '/work/.tmp': No such file or directory\n")
        settled = subprocess.CompletedProcess(["du"], 0, "8190\t/work\n", "")
        with patch.object(runtime_cases.subprocess, "run", side_effect=[raced, settled]):
            self.assertEqual(runtime_cases.worker_size(Path("/work")), 8200 * 1024)

    def test_runtime_check_reports_real_verifier_count_and_rejects_zero(self):
        task = "swebench:full:astropy__astropy-11693"
        worker = {"ws03": {"authorized": True, "hostname": "not-this-host",
                            "uid": 10250, "workspace_root": "/worker",
                            "ssh_port": 22, "ssh_target": "worker.invalid"}}
        result = {"exit": 0, "output": json.dumps({"tests_collected": 42,
                                                  "result_comparison": "REFERENCE_NOT_RUN"})}
        with patch.object(checklib, "_load_workers", return_value=worker), \
             patch.object(checklib, "run_command", return_value=result):
            command = checklib.suite_worker_gated(ROOT, "runtime", task, "ws03")[0]
        self.assertEqual(command["tests"], 42)
        self.assertEqual(command["result_comparison"], "REFERENCE_NOT_RUN")
        with patch.object(checklib, "_load_workers", return_value=worker), \
             patch.object(checklib, "run_command", return_value={"exit": 0, "output": '{"tests_collected": 0}' }):
            command = checklib.suite_worker_gated(ROOT, "runtime", task, "ws03")[0]
        self.assertEqual(command["exit"], checklib.EXIT_FAIL)


if __name__ == "__main__":
    unittest.main()
