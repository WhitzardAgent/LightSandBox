import unittest,io,json,tempfile
from pathlib import Path
from lightsandbox.cli import main
class TestCLI(unittest.TestCase):
    def call(self,args):
        out=io.StringIO();err=io.StringIO();code=main(args,stdout=out,stderr=err);return code,out.getvalue(),err.getvalue()
    def test_version_json(self):
        code,out,_=self.call(['--json','version']);self.assertEqual(code,0);self.assertFalse(json.loads(out)['runtime_implemented'])
    def test_exec_dry_run(self):
        code,out,_=self.call(['--json','exec','--generation','1','--dry-run','s','--','printf','a b;$HOME'])
        self.assertEqual(code,0);self.assertEqual(json.loads(out)['body']['command']['argv'],['printf','a b;$HOME'])
        self.assertFalse(json.loads(out)['executed'])
    def test_shell_dry_run(self):
        code,out,_=self.call(['--json','exec','--generation','1','--dry-run','--shell','echo $HOME','--shell-executable','/bin/sh','s'])
        self.assertEqual(code,0);self.assertEqual(json.loads(out)['body']['command']['kind'],'shell')
    def test_missing_generation(self):self.assertNotEqual(self.call(['exec','s','--','true'])[0],0)
    def test_dual_command(self):self.assertNotEqual(self.call(['exec','--generation','1','--dry-run','--shell','x','--shell-executable','/bin/sh','s','--','true'])[0],0)
    def test_unknown_command(self):self.assertNotEqual(self.call(['--json','harbor','verify'])[0],0)
    def test_plan_environment(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'env.json';p.write_text(json.dumps({'services':{'main':{'image':'ubuntu:24.04'}}}))
            code,out,_=self.call(['--json','env','plan','--file',str(p)])
        self.assertEqual(code,0);j=json.loads(out);self.assertFalse(j['execution_ready']);self.assertTrue(j['unresolved'])
    def test_plan_rejects_task_semantics(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'env.json';p.write_text(json.dumps({'services':{'main':{'image':'x'}},'verifier':{}}))
            self.assertNotEqual(self.call(['--json','env','plan','--file',str(p)])[0],0)

class TestPreviewPrivacy(unittest.TestCase):
    call=TestCLI.call
    def test_preview_hides_env_values(self):
        code,out,_=self.call(['--json','exec','--generation','1','--env','TOKEN=secret123','--dry-run','s','--','true'])
        self.assertEqual(code,0)
        self.assertNotIn('secret123',out)
        self.assertEqual(json.loads(out)['body']['env']['TOKEN'],'<redacted>')
