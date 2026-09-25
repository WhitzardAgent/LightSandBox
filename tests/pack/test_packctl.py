"""Tests of the documentation tooling only; they do not test a sandbox."""
from __future__ import annotations
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'tools' / 'packctl.py'

def copytree_ignore(directory, names):
    ignored = set(shutil.ignore_patterns('__pycache__', 'MANIFEST.json')(directory, names))
    # spike/uml/work is derived UML build scratch (git-ignored, multi-GB), never pack content.
    if Path(directory).relative_to(ROOT).as_posix() == 'spike/uml': ignored.add('work')
    return ignored

class PackTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / 'pack'
        shutil.copytree(ROOT, self.root, ignore=copytree_ignore)
    def tearDown(self):
        self.tmp.cleanup()
    def call(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), '--root', str(self.root), *args],
                              capture_output=True, text=True, timeout=20)
    def change(self, path, mutate):
        p=self.root/path; value=json.loads(p.read_text()); mutate(value)
        p.write_text(json.dumps(value, ensure_ascii=False, indent=2)+'\n')
    def rejected(self, fragment=''):
        r=self.call('validate'); self.assertNotEqual(r.returncode,0,r.stdout)
        self.assertIn(fragment,r.stdout+r.stderr)
    def test_unknown_feature_owner_is_rejected(self):
        self.change('specs/feature-matrix.json',lambda d:d['features'][0].update(owner_task='T99'))
        self.rejected('feature')
    def test_feature_case_mapping_is_rejected(self):
        self.change('specs/feature-matrix.json',lambda d:d['features'][0].update(test_id='T08-C99'))
        self.rejected('feature')
    def synthetic_evidence(self, zero=False, wrong_hash=False):
        # Pure synthetic fixture to exercise structural checks; not product evidence.
        log=self.root/'evidence/synthetic.log';log.write_text('synthetic fixture only')
        report={'task_id':'T00','candidate_commit':'a'*40,
                'source_lock_sha256':hashlib.sha256((self.root/'specs/upstream-lock.json').read_bytes()).hexdigest(),
                'host_report_sha256':'b'*64,'commands':[{'argv':['synthetic-fixture'],'exit_code':0,'started_at':'fixture','finished_at':'fixture'}],
                'test_ids':['T00-C01','T00-C02','T00-C03','T00-C04'],
                'collected':0 if zero else 4,'passed':0 if zero else 4,'failed':0,'blocked':0,
                'raw_artifacts':[{'path':'evidence/synthetic.log','sha256':'0'*64 if wrong_hash else hashlib.sha256(log.read_bytes()).hexdigest()}],
                'review_id':'synthetic-review'}
        (self.root/'evidence/synthetic.json').write_text(json.dumps(report))
        review={'decision':'approve','reviewer_id':'synthetic-reviewer','candidate_commit':'a'*40}
        self.change('state/project-status.json',lambda d:d['tasks'][0].update(status='verified',evidence=['evidence/synthetic.json'],review=review))
    def test_zero_tests_cannot_verify_task(self):
        self.synthetic_evidence(zero=True);self.rejected('zero')
    def test_raw_evidence_hash_is_checked(self):
        self.synthetic_evidence(wrong_hash=True);self.rejected('artifact hash')
    def test_pristine_pack_validates(self):
        r=self.call('validate');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
    def test_next_is_initial_task_only(self):
        r=self.call('next');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        payload=json.loads(r.stdout);self.assertEqual([t['id'] for t in payload['ready']],['T00'])
    def test_context_is_bounded_to_one_task(self):
        r=self.call('context','T14');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        p=json.loads(r.stdout);self.assertEqual(p['task']['id'],'T14')
        self.assertEqual(len(p['test_cases']),4);self.assertLess(len(p['read_files']),10)
    def test_unknown_task_is_error(self):
        r=self.call('context','T99');self.assertNotEqual(r.returncode,0)
    def test_duplicate_task_is_rejected(self):
        self.change('specs/task-catalog.json',lambda d:d['tasks'].append(d['tasks'][0].copy()))
        self.rejected('duplicate')
    def test_dependency_cycle_is_rejected(self):
        self.change('specs/task-catalog.json',lambda d:d['tasks'][0]['depends_on'].append('T32'))
        self.rejected('cycle')
    def test_unknown_dependency_is_rejected(self):
        self.change('specs/task-catalog.json',lambda d:d['tasks'][1]['depends_on'].append('T99'))
        self.rejected('unknown dependency')
    def test_missing_test_case_is_rejected(self):
        self.change('specs/test-catalog.json',lambda d:d['tests'].pop())
        self.rejected('test')
    def test_orphan_test_case_is_rejected(self):
        self.change('specs/test-catalog.json',lambda d:d['tests'][0].update(task_id='T99'))
        self.rejected('test')
    def test_requirement_mapping_is_checked(self):
        self.change('specs/requirements.json',lambda d:d['requirements'][0].update(tasks=[]))
        self.rejected('requirement mapping')
    def test_missing_design_reference_is_rejected(self):
        catalog=json.loads((self.root/'specs/task-catalog.json').read_text())
        task=next(t for t in catalog['tasks'] if t['id']=='T14')
        (self.root/task['read'][0]).unlink()
        self.rejected('missing')
    def test_bad_internal_link_is_rejected(self):
        with (self.root/'README.md').open('a') as f:f.write('\n[missing](not-here.md)\n')
        self.rejected('link')
    def test_verified_without_evidence_is_rejected(self):
        self.change('state/project-status.json',lambda d:d['tasks'][0].update(status='verified'))
        self.rejected('evidence')
    def test_unknown_status_is_rejected(self):
        self.change('state/project-status.json',lambda d:d['tasks'][0].update(status='looks_good'))
        self.rejected('status')
    def test_missing_task_state_is_rejected(self):
        self.change('state/project-status.json',lambda d:d['tasks'].pop())
        self.rejected('state')
    def test_evidence_path_cannot_escape_pack(self):
        self.change('state/project-status.json',lambda d:d['tasks'][0].update(status='verified',evidence=['../secret.json']))
        self.rejected('unsafe')
    def test_invalid_json_is_clear_failure(self):
        (self.root/'specs/task-catalog.json').write_text('{broken')
        self.rejected('JSON')
    def test_blueprint_is_not_releasable(self):
        r=self.call('release-check');self.assertNotEqual(r.returncode,0)
        self.assertIn('UNLOCKED',r.stdout+r.stderr)
    def test_manifest_is_required(self):
        r=self.call('verify-manifest');self.assertNotEqual(r.returncode,0)
    def seal(self):
        files={}
        for p in self.root.rglob('*'):
            # Sealing side must ignore Git internals just like verify-manifest and
            # file_set() already do; development now happens inside a git worktree.
            if p.is_file() and '__pycache__' not in p.parts and '.git' not in p.parts and p.name!='MANIFEST.json':
                b=p.read_bytes();files[p.relative_to(self.root).as_posix()]={'sha256':hashlib.sha256(b).hexdigest(),'bytes':len(b)}
        (self.root/'MANIFEST.json').write_text(json.dumps({'files':files}))
    def test_manifest_detects_modified_file(self):
        self.seal();(self.root/'README.md').write_text('modified')
        r=self.call('verify-manifest');self.assertNotEqual(r.returncode,0)
        self.assertIn('mismatch',r.stdout+r.stderr)
    def test_manifest_detects_extra_file(self):
        self.seal();(self.root/'unexpected.txt').write_text('extra')
        r=self.call('verify-manifest');self.assertNotEqual(r.returncode,0)
        self.assertIn('unexpected',r.stdout+r.stderr)
    def test_manifest_round_trip(self):
        self.seal();r=self.call('verify-manifest');self.assertEqual(r.returncode,0,r.stdout+r.stderr)

if __name__=='__main__':unittest.main()
