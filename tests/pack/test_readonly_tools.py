"""Local synthetic-fixture tests; no network and no sandbox execution."""
from __future__ import annotations
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
ROOT=Path(__file__).resolve().parents[2]

def cli(script,*args):
    return subprocess.run([sys.executable,str(ROOT/'tools'/script),*map(str,args)],capture_output=True,text=True,timeout=15)

class DoctorTests(unittest.TestCase):
    def test_doctor_never_claims_active_probe(self):
        r=cli('doctor_readonly.py');self.assertEqual(r.returncode,0,r.stderr)
        report=json.loads(r.stdout)
        self.assertEqual(report['assessment'],'NOT_PROBED')
        self.assertFalse(report['active_probe_performed'])
        self.assertEqual(report['operations'],'read_only_no_network')
    def test_output_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'report.json';p.write_text('KEEP')
            r=cli('doctor_readonly.py','--output',p)
            self.assertNotEqual(r.returncode,0);self.assertEqual(p.read_text(),'KEEP')

@unittest.skipUnless(shutil.which('git'), 'git needed for local synthetic source-lock tests')
class SourceLockTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)
        self.repo=self.root/'runc';self.repo.mkdir()
        self.git('init','-q')
        self.git('remote','add','origin','https://github.com/opencontainers/runc.git')
        (self.repo/'README.md').write_text('synthetic source-lock fixture\n')
        (self.repo/'docs').mkdir();(self.repo/'docs/terminals.md').write_text('synthetic terminal fixture\n')
        self.git('add','.');self.git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture')
    def tearDown(self):self.tmp.cleanup()
    def git(self,*args):
        r=subprocess.run(['git','-C',str(self.repo),*args],capture_output=True,text=True,timeout=10)
        self.assertEqual(r.returncode,0,r.stderr);return r.stdout.strip()
    def call(self,*args):return cli('lock_local_sources.py','--repo',f'runc={self.repo}',*args)
    def test_clean_clone_produces_actual_hash(self):
        r=self.call();self.assertEqual(r.returncode,0,r.stderr)
        report=json.loads(r.stdout);self.assertEqual(report['status'],'PARTIAL')
        item=next(x for x in report['repositories'] if x['name']=='runc')
        self.assertEqual(item['revision'],self.git('rev-parse','HEAD'))
        self.assertEqual(item['file_hashes']['README.md'],hashlib.sha256((self.repo/'README.md').read_bytes()).hexdigest())
    def test_dirty_source_is_rejected(self):
        (self.repo/'README.md').write_text('dirty')
        r=self.call();self.assertNotEqual(r.returncode,0);self.assertIn('dirty',r.stderr+r.stdout)
    def test_missing_required_source_is_rejected(self):
        self.git('rm','docs/terminals.md');self.git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','remove fixture')
        r=self.call();self.assertNotEqual(r.returncode,0);self.assertIn('missing required',r.stderr+r.stdout)
    def test_require_all_rejects_partial_lock(self):
        r=self.call('--require-all');self.assertNotEqual(r.returncode,0)
    def test_wrong_origin_is_rejected(self):
        self.git('remote','set-url','origin','https://github.com/example/unrelated.git')
        r=self.call();self.assertNotEqual(r.returncode,0);self.assertIn('origin mismatch',r.stderr+r.stdout)
    def test_unknown_repo_is_rejected(self):
        r=cli('lock_local_sources.py','--repo',f'not_registered={self.repo}')
        self.assertNotEqual(r.returncode,0)

if __name__=='__main__':unittest.main()
