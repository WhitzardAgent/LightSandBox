import unittest, httpx, asyncio
from lightsandbox import Client, AsyncClient, SandboxRef, ExecSpec
from lightsandbox.client import ApiError, AmbiguousRequest, WaitTimeout, OperationUnknown

class TestSyncClient(unittest.TestCase):
    def test_create_route_and_key(self):
        seen=[]
        def handler(r):
            seen.append(r);return httpx.Response(202,json={'id':'op_a','status':'pending'})
        with Client('https://node.invalid',transport=httpx.MockTransport(handler)) as c:
            self.assertEqual(c.sandboxes.create({'image':'ubuntu'},idempotency_key='k')['id'],'op_a')
        self.assertEqual(seen[0].url.path,'/v1/sandboxes');self.assertEqual(seen[0].headers['Idempotency-Key'],'k')
    def test_no_mutation_retry(self):
        n=[]
        def fail(r):n.append(r);raise httpx.ReadTimeout('lost',request=r)
        with Client('https://node.invalid',transport=httpx.MockTransport(fail)) as c:
            with self.assertRaises(AmbiguousRequest) as e:c.sandboxes.create({'image':'x'},idempotency_key='same')
        self.assertEqual(len(n),1);self.assertEqual(e.exception.idempotency_key,'same')
    def test_get_transport_not_mutation(self):
        def fail(r):raise httpx.ConnectError('down',request=r)
        with Client('https://node.invalid',transport=httpx.MockTransport(fail)) as c:
            with self.assertRaises(ApiError) as e:c.sandboxes.get(SandboxRef('s',1))
        self.assertNotIsInstance(e.exception,AmbiguousRequest)
    def test_generation_header(self):
        seen=[]
        def handler(r):seen.append(r);return httpx.Response(200,json={'id':'s'})
        with Client('https://node.invalid',transport=httpx.MockTransport(handler)) as c:c.sandboxes.get(SandboxRef('s',7))
        self.assertEqual(seen[0].headers['X-LS-Generation'],'7')
    def test_error_payload(self):
        def h(r):return httpx.Response(409,json={'error':{'code':'STALE_GENERATION','message':'old','request_id':'rq1'}})
        with Client('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            with self.assertRaises(ApiError) as e:c.sandboxes.get(SandboxRef('s',1))
        self.assertEqual(e.exception.code,'STALE_GENERATION');self.assertEqual(e.exception.request_id,'rq1')
    def test_redirect_not_followed(self):
        seen=[]
        def h(r):seen.append(r);return httpx.Response(307,headers={'location':'https://other.invalid'})
        with Client('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            with self.assertRaises(ApiError):c.sandboxes.get(SandboxRef('s',1))
        self.assertEqual(len(seen),1)
    def test_connection_close_does_not_delete(self):
        seen=[]
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:seen.append(r))) as c:pass
        self.assertEqual(seen,[])
    def test_wait_success(self):
        states=iter([{'id':'o','status':'running'},{'id':'o','status':'succeeded','result':{'sandbox_id':'s','generation':1}}])
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(200,json=next(states)))) as c:
            self.assertEqual(c.operations.wait('o',timeout_ms=100,poll_ms=1)['result']['sandbox_id'],'s')
    def test_unknown_operation(self):
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(200,json={'id':'o','status':'unknown'}))) as c:
            with self.assertRaises(OperationUnknown):c.operations.wait('o',timeout_ms=100)
    def test_wait_timeout_no_cancel(self):
        seen=[]
        def h(r):seen.append(r);return httpx.Response(200,json={'id':'o','status':'running'})
        with Client('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            with self.assertRaises(WaitTimeout):c.operations.wait('o',timeout_ms=2,poll_ms=1)
        self.assertTrue(seen);self.assertTrue(all(r.method=='GET' for r in seen))
    def test_exec_argv_payload(self):
        import json
        seen=[]
        def h(r):seen.append(r);return httpx.Response(202,json={'id':'x','state':'accepted'})
        with Client('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            c.executions.start(SandboxRef('s',1),ExecSpec(argv=['echo','a b']),idempotency_key='k')
        self.assertEqual(json.loads(seen[0].content)['command']['argv'],['echo','a b'])
    def test_no_plain_http_remote_token(self):
        with self.assertRaises(ValueError):Client('http://remote.invalid',token='secret')
    def test_bad_endpoint(self):
        for e in ('file:///etc/passwd','https://user:pw@host','https://host/path','https://host?x=y'):
            with self.subTest(e=e),self.assertRaises(ValueError):Client(e)
    def test_future_status_rejected(self):
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(200,json={'id':'o','status':'magic'}))) as c:
            with self.assertRaises(ApiError):c.operations.wait('o',timeout_ms=100)
    def test_id_path_injection(self):
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(200,json={}))) as c:
            with self.assertRaises(ValueError):c.operations.get('../secret')

class TestAsyncClient(unittest.IsolatedAsyncioTestCase):
    async def test_async_same_contract(self):
        seen=[]
        async def h(r):seen.append(r);return httpx.Response(202,json={'id':'x','state':'accepted'})
        async with AsyncClient('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            await c.executions.start(SandboxRef('s',2),ExecSpec(argv=['printf','x']),idempotency_key='k')
        self.assertEqual(seen[0].headers['X-LS-Generation'],'2')
    async def test_cancellation_only_wait(self):
        seen=[]
        async def h(r):seen.append(r);return httpx.Response(200,json={'id':'o','status':'running'})
        async with AsyncClient('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            task=asyncio.create_task(c.operations.wait('o',timeout_ms=10000,poll_ms=1))
            await asyncio.sleep(.01);task.cancel()
            with self.assertRaises(asyncio.CancelledError):await task
        self.assertTrue(all(r.method=='GET' for r in seen))
    async def test_delete_is_operation(self):
        async with AsyncClient('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(202,json={'id':'d','status':'pending'}))) as c:
            self.assertEqual((await c.sandboxes.delete(SandboxRef('s',1),idempotency_key='d'))['status'],'pending')

class TestRefAndErrorValidation(unittest.TestCase):
    def test_environment_query_is_fenced(self):
        seen=[]
        def h(r):seen.append(r);return httpx.Response(200,json={'id':'env_a','generation':3})
        with Client('https://node.invalid',transport=httpx.MockTransport(h)) as c:
            c.environments.get('env_a',generation=3)
        self.assertEqual(seen[0].headers['X-LS-Generation'],'3')
    def test_bad_operation_error_becomes_protocol_error(self):
        with Client('https://node.invalid',transport=httpx.MockTransport(lambda r:httpx.Response(200,json={'status':'failed','error':'invalid'}))) as c:
            with self.assertRaises(ApiError) as err:c.operations.wait('o')
        self.assertEqual(err.exception.code,'PROTOCOL_ERROR')
