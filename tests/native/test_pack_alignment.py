"""References between the delivered native contract, code, and current task graph."""
import json,unittest,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def read(p):return json.loads((ROOT/p).read_text())
class TestCurrentPackAlignment(unittest.TestCase):
    def test_no_task_reads_obsolete_specs(self):
        for t in read('specs/task-catalog.json')['tasks']:
            for p in t['read']:
                self.assertFalse(p.startswith(('history/','docs/design/','docs/harbor/')),(t['id'],p))
                self.assertTrue((ROOT/p).is_file(),p)
    def test_template_reset_not_blocked_by_snapshot(self):
        tasks={t['id']:t for t in read('specs/task-catalog.json')['tasks']}
        self.assertNotIn('T22',tasks['T38']['depends_on'])
        self.assertIn('T41',tasks['T04']['depends_on'])
        self.assertIn('T43',tasks['T21']['depends_on'])
    def test_client_declared_and_actual_version(self):
        import lightsandbox
        spec=tomllib.loads((ROOT/'client/python/pyproject.toml').read_text())
        self.assertEqual(spec['project']['version'],lightsandbox.__version__)
        self.assertEqual(spec['project']['scripts']['lsbx'],'lightsandbox.cli:main')
    def test_no_runtime_claims(self):
        state=read('state/project-status.json')
        self.assertTrue(all(t['status']=='planned' for t in state['tasks']))
        self.assertTrue(all(r['server_status']=='not-implemented' for r in read('contracts/v1/surface.json')['http']))
    def test_migration_targets_exist(self):
        for d in read('specs/v4-migration-index.json')['documents']:
            self.assertTrue((ROOT/d['archive_path']).is_file())
            for p in d['replaced_by']:self.assertTrue((ROOT/p).is_file())
