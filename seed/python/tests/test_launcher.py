import unittest
from lightsandbox_harbor.launcher import build_argv, validate_lock
from pathlib import Path
from tempfile import TemporaryDirectory
import hashlib

class LauncherTests(unittest.TestCase):
    def test_forces_provider(self):
        argv=build_argv(task=Path('/tmp/a task'),agent='oracle')
        self.assertEqual(argv,['harbor','run','-p','/tmp/a task','--env','lightsandbox_harbor.provider:LightSandboxEnvironment','-a','oracle'])
    def test_dataset_uses_upstream_resolution(self):
        self.assertEqual(build_argv(dataset='terminal-bench@2.0')[2:4],['-d','terminal-bench@2.0'])
    def test_requires_exactly_one_source(self):
        for kwargs in [{},{'task':Path('a'),'dataset':'b'}]:
            with self.assertRaises(ValueError):build_argv(**kwargs)
    def test_rejects_flag_injection(self):
        with self.assertRaises(ValueError):build_argv(dataset='--env=docker')
    def test_no_shell_interpolation(self):
        self.assertIn('/tmp/a;echo x',build_argv(task=Path('/tmp/a;echo x')))
    def test_unlocked_refused(self):
        with TemporaryDirectory() as d:
            with self.assertRaises(ValueError):validate_lock(Path(d),{'status':'UNLOCKED'})
    def test_missing_contract_files_refused(self):
        with TemporaryDirectory() as d:
            with self.assertRaises(ValueError):validate_lock(Path(d),{'status':'LOCKED','files':{}})
