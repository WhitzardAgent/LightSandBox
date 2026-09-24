import unittest
from lightsandbox_seed.leases import Capacity, LeasePool, LeaseError

class LeaseTests(unittest.TestCase):
    def setUp(self): self.p = LeasePool(Capacity(4000, 8192, 4))
    def test_acquire(self):
        x=self.p.acquire("a", "r1", Capacity(1000, 1024, 1)); self.assertEqual(x.generation,1)
    def test_idempotent(self):
        a=self.p.acquire("a","r",Capacity(1000,1024,1)); self.assertEqual(a,self.p.acquire("a","r",Capacity(1000,1024,1)))
    def test_conflicting_request(self):
        self.p.acquire("a","r",Capacity(1000,1024,1))
        with self.assertRaises(LeaseError): self.p.acquire("a","r",Capacity(2000,1024,1))
    def test_capacity_exhaustion(self):
        self.p.acquire("a","r",Capacity(4000,8192,4))
        with self.assertRaises(LeaseError): self.p.acquire("a","s",Capacity(1,1,1))
    def test_wrong_owner(self):
        a=self.p.acquire("a","r",Capacity(1,1,1))
        with self.assertRaises(LeaseError): self.p.check("b",a.lease_id,a.generation)
    def test_drain_blocks_commands(self):
        a=self.p.acquire("a","r",Capacity(1,1,1)); self.p.begin_release("a",a.lease_id,1)
        with self.assertRaises(LeaseError): self.p.check("a",a.lease_id,1)
    def test_drain_not_capacity_release(self):
        a=self.p.acquire("a","r",Capacity(4000,8192,4)); self.p.begin_release("a",a.lease_id,1)
        with self.assertRaises(LeaseError): self.p.acquire("a","s",Capacity(1,1,1))
    def test_cleanup_receipt_required(self):
        a=self.p.acquire("a","r",Capacity(1,1,1)); self.p.begin_release("a",a.lease_id,1)
        with self.assertRaises(LeaseError): self.p.confirm_clean("a",a.lease_id,1,"")
    def test_release_reacquire_no_reused_identity(self):
        a=self.p.acquire("a","r",Capacity(1,1,1)); self.p.begin_release("a",a.lease_id,1)
        self.p.confirm_clean("a",a.lease_id,1,"sha256:"+"a"*64)
        b=self.p.acquire("a","s",Capacity(1,1,1)); self.assertNotEqual(a.lease_id,b.lease_id)
        with self.assertRaises(LeaseError): self.p.check("a",a.lease_id,1)
    def test_request_key_not_recycled(self):
        a=self.p.acquire("a","r",Capacity(1,1,1)); self.p.begin_release("a",a.lease_id,1)
        self.p.confirm_clean("a",a.lease_id,1,"sha256:"+"a"*64)
        with self.assertRaises(LeaseError): self.p.acquire("a","r",Capacity(1,1,1))
    def test_stale_generation(self):
        a=self.p.acquire("a","r",Capacity(1,1,1))
        with self.assertRaises(LeaseError): self.p.check("a",a.lease_id,2)
    def test_invalid_resources(self):
        for args in [(True,1,1),(0,1,1),(1,-1,1),(1,1,0)]:
            with self.assertRaises(ValueError): Capacity(*args)
    def test_concurrent_capacity(self):
        from concurrent.futures import ThreadPoolExecutor
        def run(i):
            try:return self.p.acquire("a",str(i),Capacity(1000,1024,1))
            except LeaseError:return None
        with ThreadPoolExecutor(max_workers=12) as pool: results=list(pool.map(run,range(40)))
        self.assertEqual(sum(x is not None for x in results),4)
