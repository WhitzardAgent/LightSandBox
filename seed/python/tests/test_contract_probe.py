import unittest
from lightsandbox_harbor.contract_probe import analyze_base_source

class ContractProbeTests(unittest.TestCase):
    def test_missing_class(self):
        self.assertFalse(analyze_base_source('class Wrong: pass')['compatible_shape'])
    def test_missing_method(self):
        r=analyze_base_source('class BaseEnvironment:\n    async def start(self, force_build): pass\n')
        self.assertFalse(r['compatible_shape']); self.assertIn('exec',r['missing_methods'])
    def test_does_not_execute_input(self):
        r=analyze_base_source('raise RuntimeError("DO NOT EXECUTE")\nclass BaseEnvironment: pass\n')
        self.assertFalse(r['compatible_shape'])
    def test_invalid_syntax(self):
        with self.assertRaises(ValueError):analyze_base_source('class !!!')
