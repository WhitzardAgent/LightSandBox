"""Contract doubles only: this suite does NOT install or run real Harbor."""
import base64
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest

from lightsandbox_harbor.bridge import Handle
from lightsandbox_harbor.provider_core import make_environment_type

class Model:
    def __init__(self, **kwargs): self.__dict__.update(kwargs)
    def model_dump(self, **kwargs): return vars(self).copy()

class HarborBaseDouble:
    def __init__(self, environment_dir, environment_name, session_id, trial_paths, task_env_config, **kwargs):
        self.environment_dir=Path(environment_dir); self.environment_name=environment_name
        self.session_id=session_id; self.trial_paths=trial_paths; self.task_env_config=task_env_config
        self._persistent_env=kwargs.get('persistent_env',{});self.default_user='agent'
        self.network_policy=Model(network_mode='public',allowed_hosts=[])
        self._phase_network_policies=[];self._mounts=[];self.extra_docker_compose_paths=[]
        self._cpu_resource_mode=SimpleNamespace(value='auto');self._memory_resource_mode=SimpleNamespace(value='auto')
        self._validate_definition()
    def _startup_env(self): return dict(self._persistent_env)
    def _merge_env(self,env): return {**self._persistent_env,**(env or {}),'SCOPE':'applied'}
    def _resolve_user(self,user): return user if user is not None else self.default_user
    def _output_callback(self): return None
    async def _upload_environment_dir_after_start(self): self.payload_helper_called=True

class Backend:
    def __init__(self): self.calls=[];self.delete_fails=False;self.create_fails=False
    async def start(self,request):
        self.calls.append(('start',request))
        if self.create_fails: raise RuntimeError('not admitted')
        return Handle('sandbox-a',1)
    async def stop(self,handle,delete):
        self.calls.append(('stop',handle,delete))
        if self.delete_fails: raise RuntimeError('cleanup incomplete')
    async def events(self,handle,request):
        self.calls.append(('exec',request))
        yield dict(run_id=request.execution_id,generation=handle.generation,sequence=1,kind='exit',return_code=0)
    async def cancel(self,handle,execution_id): self.calls.append(('cancel',execution_id))
    async def transfer(self,handle,request): self.calls.append(('transfer',request))

class ProviderTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp=TemporaryDirectory();self.root=Path(self.tmp.name);(self.root/'Dockerfile').write_text('FROM busybox\n')
        self.backend=Backend();self.cls=make_environment_type(HarborBaseDouble,Model,Model,Model)
        self.env=self.cls(self.root,'task','trial',None,Model(docker_image=None),backend=self.backend,persistent_env={'A':'1'})
    async def asyncTearDown(self):self.tmp.cleanup()
    async def test_is_subclass(self):self.assertIsInstance(self.env,HarborBaseDouble)
    async def test_start_invokes_upstream_payload_hook(self):
        await self.env.start(False);self.assertTrue(self.env.payload_helper_called)
    async def test_start_keeps_source_directory(self):
        await self.env.start(False);self.assertEqual(self.backend.calls[0][1].environment_dir,self.root)
    async def test_forward_start_env(self):
        await self.env.start(False);self.assertEqual(self.backend.calls[0][1].startup_env,{'A':'1'})
    async def test_no_exec_before_start(self):
        with self.assertRaises(RuntimeError):await self.env.exec('true')
    async def test_preserves_env_and_numeric_root(self):
        await self.env.start(False);result=await self.env.exec('id',env={'A':'2'},user=0)
        req=self.backend.calls[-1][1];self.assertEqual(req.user,0);self.assertEqual(req.env['SCOPE'],'applied');self.assertEqual(req.env['A'],'2');self.assertEqual(result.return_code,0)
    async def test_default_user(self):
        await self.env.start(False);await self.env.exec('id');self.assertEqual(self.backend.calls[-1][1].user,'agent')
    async def test_repeat_start_no_second_creation(self):
        await self.env.start(False);await self.env.start(False);self.assertEqual(sum(c[0]=='start' for c in self.backend.calls),1)
    async def test_stop_keep_retains_handle_but_blocks_exec(self):
        await self.env.start(False);await self.env.stop(False)
        with self.assertRaises(RuntimeError):await self.env.exec('true')
        self.assertIsNotNone(self.env._handle)
    async def test_delete_failure_keeps_handle_for_reconciliation(self):
        await self.env.start(False);self.backend.delete_fails=True
        with self.assertRaises(RuntimeError):await self.env.stop(True)
        self.assertIsNotNone(self.env._handle)
    async def test_failed_start_not_ready(self):
        self.backend.create_fails=True
        with self.assertRaises(RuntimeError):await self.env.start(False)
        with self.assertRaises(RuntimeError):await self.env.exec('true')
    async def test_transfer_direction(self):
        await self.env.start(False);await self.env.download_file('/tmp/out',self.root/'out')
        req=self.backend.calls[-1][1];self.assertEqual((req.direction,req.kind,req.guest_path),('download','file','/tmp/out'))
    async def test_no_host_mounted_claim(self):self.assertFalse(self.env.capabilities.mounted)
    async def test_compose_seed_rejects_explicitly(self):
        (self.root/'docker-compose.yaml').write_text('services: {}\n')
        with self.assertRaisesRegex(ValueError,'Compose'):self.cls(self.root,'task','trial',None,Model(),backend=self.backend)
