"""Real offline parser tests. Fixtures are synthetic, not benchmark runs."""
from pathlib import Path
from tempfile import TemporaryDirectory
import os, unittest
import lightsandbox_seed

class HarborLoaderTests(unittest.TestCase):
    def setUp(self):
        self.tmp=TemporaryDirectory();self.root=Path(self.tmp.name)
        (self.root/'environment').mkdir();(self.root/'tests').mkdir();(self.root/'solution').mkdir()
        (self.root/'instruction.md').write_text('Create /app/answer.txt')
        (self.root/'task.toml').write_text('schema_version="1.4"\n[task]\nname="local/test"\n[environment]\ncpus=2\nmemory_mb=1024\n')
        (self.root/'environment/Dockerfile').write_text('FROM python:3.12-slim\nWORKDIR /app\n')
        (self.root/'tests/test.sh').write_text('test -f /app/answer.txt')
        (self.root/'solution/solve.sh').write_text('touch /app/answer.txt')
    def tearDown(self): self.tmp.cleanup()
    def load(self, **kw):
        from lightsandbox_seed.harbor_loader import inspect_task
        return inspect_task(self.root,**kw)
    def configure(self, extra):
        with (self.root/'task.toml').open('a') as f:f.write(extra)
    def test_loader_api_exists(self):
        import importlib.util
        self.assertIsNotNone(importlib.util.find_spec('lightsandbox_seed.harbor_loader'))
    def test_dockerfile_plan(self):
        p=self.load();self.assertEqual(p.agent.kind,'dockerfile');self.assertEqual(p.name,'local/test')
        self.assertFalse(p.authoritative_validation);self.assertEqual(p.agent.resources['cpus'],2)
    def test_prebuilt_preferred_without_force(self):
        self.configure('docker_image="registry/image:tag"\n')
        self.assertEqual(self.load().agent.image_selection,'prebuilt')
    def test_force_build_selects_dockerfile(self):
        self.configure('docker_image="registry/image:tag"\n')
        self.assertEqual(self.load(force_build=True).agent.image_selection,'build')
    def test_force_build_without_dockerfile_keeps_image(self):
        (self.root/'environment/Dockerfile').unlink();self.configure('docker_image="registry/image:tag"\n')
        self.assertEqual(self.load(force_build=True).agent.image_selection,'prebuilt')
    def test_prebuilt_payload_uploaded_after_start(self):
        (self.root/'environment/Dockerfile').unlink();self.configure('docker_image="registry/image:tag"\n')
        (self.root/'environment/input.txt').write_text('data')
        self.assertTrue(self.load().agent.upload_payload)
    def test_compose_is_not_flattened(self):
        (self.root/'environment/docker-compose.yaml').write_text('services:\n  db:\n    image: postgres:16\n')
        p=self.load();self.assertEqual(p.agent.kind,'compose');self.assertIn('compose_normalization',p.required_features)
    def test_agent_payload_never_includes_tests_solution(self):
        p=self.load();names=[r.path for r in p.agent.entries]
        self.assertNotIn('tests/test.sh',names);self.assertNotIn('solution/solve.sh',names)
    def test_no_definition_fails(self):
        (self.root/'environment/Dockerfile').unlink()
        with self.assertRaises(ValueError):self.load()
    def test_unknown_fields_are_reported_not_dropped(self):
        self.configure('mystery_resource=100\n')
        p=self.load();self.assertIn('mystery_resource',p.agent.unknown_fields)
        self.assertFalse(p.execution_ready)
    def test_separate_verifier_keeps_own_source(self):
        self.configure('[verifier]\nenvironment_mode="separate"\n[verifier.environment]\ndocker_image="grader:1"\n')
        p=self.load();self.assertEqual(p.verifier_mode,'separate');self.assertEqual(p.verifier.source_role,'verifier')
        self.assertEqual(p.verifier.image,'grader:1')
    def test_separate_verifier_infers_mode(self):
        self.configure('[verifier.environment]\ndocker_image="grader:1"\n')
        self.assertEqual(self.load().verifier_mode,'separate')
    def test_shared_with_verifier_image_rejected(self):
        self.configure('[verifier]\nenvironment_mode="shared"\n[verifier.environment]\ndocker_image="grader:1"\n')
        with self.assertRaises(ValueError):self.load()
    def test_separate_without_config_records_fresh_agent_spec(self):
        self.configure('[verifier]\nenvironment_mode="separate"\n')
        # Harbor uses verifier source resolution, not an ad-hoc clone of agent files.
        p=self.load();self.assertIsNone(p.verifier);self.assertIn('verifier_source_resolution',p.required_features)
    def test_negative_resource_rejected(self):
        p=self.root/'task.toml';p.write_text(p.read_text().replace('cpus=2','cpus=-1'))
        with self.assertRaises(ValueError):self.load()
    def test_bool_is_not_cpu_count(self):
        p=self.root/'task.toml';p.write_text(p.read_text().replace('cpus=2','cpus=true'))
        with self.assertRaises(ValueError):self.load()
    def test_windows_is_visible_blocker(self):
        self.configure('os="windows"\n');p=self.load()
        self.assertIn('windows',p.required_features);self.assertFalse(p.execution_ready)
    def test_multistep_delegated(self):
        self.configure('[[steps]]\nname="stage-one"\n');p=self.load()
        self.assertIn('harbor_multistep_orchestration',p.required_features)
    def test_secrets_values_not_in_report(self):
        self.configure('env={API_KEY="DO_NOT_LEAK_123"}\n')
        text=self.load().to_json();self.assertNotIn('DO_NOT_LEAK_123',text);self.assertIn('API_KEY',text)
    def test_schema_version_not_assumed_from_python_package(self):
        p=self.root/'task.toml';p.write_text(p.read_text().replace('"1.4"','"42.0"'))
        result=self.load();self.assertIn('upstream_schema_validation',result.required_features)
    def test_untrusted_symlink_is_not_read(self):
        (self.root/'environment/leak').symlink_to('/etc/passwd')
        with self.assertRaises(ValueError):self.load()
    def test_symlinked_task_config_rejected(self):
        old=self.root/'task.toml';target=self.root/'real.toml';old.rename(target);old.symlink_to(target)
        with self.assertRaises(ValueError):self.load()
