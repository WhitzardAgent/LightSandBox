"""Offline v5 contract tests. No kernel/container claims."""
import unittest
from lightsandbox.contracts import (SandboxRef, ExecSpec, ContractError, ExitStatus,
    resolve_launch, validate_environment, allowed_transition)

class TestContract(unittest.TestCase):
    def test_reference(self):
        self.assertEqual(SandboxRef('sb_1',1).to_dict(),{'sandbox_id':'sb_1','generation':1})
    def test_bad_identity(self):
        for s in ('../foo','a/b','','a?x=2','a%2fb'):
            with self.subTest(s=s),self.assertRaises(ContractError): SandboxRef(s,1)
    def test_bad_generation(self):
        for g in (True,0,-1,1.0,'1'):
            with self.subTest(g=g),self.assertRaises(ContractError): SandboxRef('s',g)
    def test_argv_not_shell(self):
        s=ExecSpec(argv=['printf','%s','a; touch /tmp/x'])
        self.assertEqual(s.to_dict()['command'],{'kind':'argv','argv':['printf','%s','a; touch /tmp/x']})
    def test_shell_explicit(self):
        s=ExecSpec(shell='echo $HOME',shell_executable='/bin/bash')
        self.assertEqual(s.to_dict()['command'],{'kind':'shell','executable':'/bin/bash','script':'echo $HOME'})
    def test_shell_no_default(self):
        with self.assertRaises(ContractError): ExecSpec(shell='echo x')
    def test_exactly_one_mode(self):
        for kw in ({},{'argv':['ls'],'shell':'ls','shell_executable':'/bin/sh'}):
            with self.subTest(kw=kw),self.assertRaises(ContractError):ExecSpec(**kw)
    def test_argv_nonempty(self):
        for a in ([],[''],['x','a\0b'],'ls'):
            with self.subTest(a=a),self.assertRaises(ContractError):ExecSpec(argv=a)
    def test_empty_argument_preserved(self):
        self.assertEqual(ExecSpec(argv=['printf','']).to_dict()['command']['argv'][1],'')
    def test_duration(self):
        for n in (True,0,-1,1.5,float('nan')):
            with self.subTest(n=n),self.assertRaises(ContractError):ExecSpec(argv=['x'],timeout_ms=n)
    def test_env(self):
        for d in ({'':'x'},{'A=B':'x'},{'X':'a\0b'},{'X':None}):
            with self.subTest(d=d),self.assertRaises(ContractError):ExecSpec(argv=['x'],env=d)
    def test_cwd(self):
        with self.assertRaises(ContractError):ExecSpec(argv=['x'],cwd='relative')
    def test_snapshot_input(self):
        a=['x'];d={'A':'B'};s=ExecSpec(argv=a,env=d);a.append('y');d['A']='Z'
        self.assertEqual(s.to_dict()['command']['argv'],['x']);self.assertEqual(s.to_dict()['env'],{'A':'B'})
    def test_exit_zero(self):self.assertEqual(ExitStatus('exited',exit_code=0).normalized_code(),0)
    def test_nonzero_is_observed_exit(self):self.assertEqual(ExitStatus('exited',exit_code=42).normalized_code(),42)
    def test_signal_separate(self):self.assertEqual(ExitStatus('signaled',signal=9).normalized_code(),137)
    def test_unknown_has_no_exit(self):
        with self.assertRaises(ContractError):ExitStatus('unknown').normalized_code()
    def test_exit_disjoint(self):
        for kw in ({'kind':'exited','exit_code':0,'signal':9},{'kind':'unknown','exit_code':0},{'kind':'signaled','signal':True}):
            with self.subTest(kw=kw),self.assertRaises(ContractError):ExitStatus(**kw)
    def test_entrypoint_cmd(self):
        r=resolve_launch({'Entrypoint':['/entry'],'Cmd':['a'],'WorkingDir':'/app','Env':['X=1'],'User':'app'})
        self.assertEqual(r['argv'],['/entry','a']);self.assertEqual(r['cwd'],'/app');self.assertEqual(r['user'],'app')
    def test_null_inherit_empty_clear(self):
        r=resolve_launch({'Entrypoint':['/entry'],'Cmd':['a']},entrypoint=[])
        self.assertEqual(r['argv'],['a'])
    def test_command_override(self):
        self.assertEqual(resolve_launch({'Entrypoint':['/e'],'Cmd':['a']},command=['b'])['argv'],['/e','b'])
    def test_nothing_to_run(self):
        with self.assertRaises(ContractError):resolve_launch({})
    def test_idle_explicit(self):
        r=resolve_launch({'Entrypoint':['/init'],'Cmd':['a']},mode='idle-agent')
        self.assertEqual(r['argv'],['/.lightsandbox/bin/init','--idle']);self.assertEqual(r['ignored_image_argv'],['/init','a'])
    def test_native_core_rejects_verifier(self):
        with self.assertRaises(ContractError):validate_environment({'services':{'main':{'image':'sha256:'+'a'*64}},'verifier':{}})
    def test_environment_valid(self):
        x={'schema_version':'v1','services':{'main':{'image':'sha256:'+'a'*64,'launch_mode':'image'}},'networks':{},'volumes':{}}
        self.assertEqual(validate_environment(x),x)
    def test_no_host_mount(self):
        with self.assertRaises(ContractError):validate_environment({'services':{'main':{'image':'x','mounts':[{'host_path':'/'}]}}})
    def test_no_silent_unknown_field(self):
        with self.assertRaises(ContractError):validate_environment({'services':{'main':{'image':'x','magic':True}}})
    def test_state_domains(self):
        self.assertTrue(allowed_transition('execution','running','exited'))
        self.assertFalse(allowed_transition('execution','exited','running'))
        self.assertFalse(allowed_transition('execution','unknown','starting'))
        self.assertTrue(allowed_transition('operation','unknown','succeeded',evidence=True))
        self.assertFalse(allowed_transition('operation','unknown','succeeded'))
        self.assertFalse(allowed_transition('sandbox','paused','deleted'))

class TestStrictInputs(unittest.TestCase):
    def test_falsey_env_not_silently_normalized(self):
        for v in ([], '', False, 0):
            with self.subTest(v=v), self.assertRaises(ContractError):
                ExecSpec(argv=['true'], env=v)
    def test_falsey_image_fields_rejected(self):
        for k, v in [('Entrypoint',''),('Cmd',False),('Env',{}),('WorkingDir',0),('User',False)]:
            with self.subTest(k=k,v=v), self.assertRaises(ContractError):
                resolve_launch({'Cmd':['true'], k:v})
    def test_image_env_string_list(self):
        for v in ('A=B',[None],['A=bad\0value']):
            with self.subTest(v=v), self.assertRaises(ContractError):
                resolve_launch({'Cmd':['true'],'Env':v})
