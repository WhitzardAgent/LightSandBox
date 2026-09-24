import asyncio, base64, unittest
from lightsandbox_harbor.bridge import Handle, Command, ExecutionBridge, BackendError

class FakeBackend:
    def __init__(self):self.calls=[];self.block=False;self.truncate=False
    async def events(self, handle, request):
        self.calls.append((handle,request))
        if self.block: await asyncio.sleep(20)
        for seq,data in enumerate([b"\xe4\xbd", b"\xa0"],1):
            yield dict(run_id=request.execution_id,generation=handle.generation,sequence=seq,kind="output",stream="stdout",data=base64.b64encode(data).decode())
        if not self.truncate:
            yield dict(run_id=request.execution_id,generation=handle.generation,sequence=3,kind="exit",return_code=0)
    async def cancel(self,handle,execution_id): self.calls.append(("cancel",execution_id))

class BridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):self.backend=FakeBackend(); self.bridge=ExecutionBridge(self.backend);self.handle=Handle("sandbox-a",1)
    async def test_stream_decode_cross_chunks(self):
        output=[]
        async def cb(text,stream):output.append((text,stream))
        result=await self.bridge.exec(self.handle,Command("e1","echo hi"),cb)
        self.assertEqual(result.stdout,"你");self.assertEqual("".join(x[0] for x in output),"你")
    async def test_return_zero(self):
        self.assertEqual((await self.bridge.exec(self.handle,Command("e1","true"))).return_code,0)
    async def test_user_zero_preserved(self):
        await self.bridge.exec(self.handle,Command("e1","id",user=0));self.assertEqual(self.backend.calls[0][1].user,0)
    async def test_missing_receipt_is_error(self):
        self.backend.truncate=True
        with self.assertRaises(BackendError): await self.bridge.exec(self.handle,Command("e1","true"))
    async def test_cancel_does_not_return_success(self):
        self.backend.block=True
        task=asyncio.create_task(self.bridge.exec(self.handle,Command("e1","sleep 20")))
        await asyncio.sleep(.01); task.cancel()
        with self.assertRaises(asyncio.CancelledError): await task
        self.assertEqual(self.backend.calls[-1],("cancel","e1"))
    async def test_timeout_propagates_and_cancels(self):
        self.backend.block=True
        with self.assertRaises(TimeoutError): await self.bridge.exec(self.handle,Command("e1","sleep",timeout_sec=.01))
        self.assertEqual(self.backend.calls[-1],("cancel","e1"))
    async def test_invalid_timeout(self):
        for value in [0,-1,True,float("nan")]:
            with self.assertRaises(ValueError): Command("e1","true",timeout_sec=value)
    async def test_env_is_copied(self):
        env={"A":"1"};req=Command("e1","env",env=env);env["A"]="2"
        self.assertEqual(req.env["A"],"1")
    async def test_invalid_handle(self):
        for identity,gen in [("",1),("id",0),("id",True)]:
            with self.assertRaises(ValueError): Handle(identity,gen)
