from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

class JournalTests(unittest.TestCase):
    def setUp(self):
        from lightsandbox_seed.journal import Journal
        self.tmp=TemporaryDirectory();self.path=Path(self.tmp.name)/'state.db';self.j=Journal(self.path)
    def tearDown(self):self.j.close();self.tmp.cleanup()
    def begin(self,key='k1',body=None,owner='tenant1'):
        return self.j.begin(owner,key,body or {'command':'echo hi'})
    def test_begin_is_durable_intent(self):
        a=self.begin();self.assertEqual(a.status,'PREPARED');self.assertTrue(a.new)
    def test_same_request_reuses_same_id(self):
        a=self.begin();b=self.begin();self.assertEqual(a.operation_id,b.operation_id);self.assertFalse(b.new)
    def test_key_conflict_rejected(self):
        self.begin()
        with self.assertRaisesRegex(ValueError,'conflict'):self.begin(body={'command':'different'})
    def test_tenant_keys_do_not_collide(self):
        a=self.begin();b=self.begin(owner='tenant2');self.assertNotEqual(a.operation_id,b.operation_id)
    def test_state_compare_and_swap(self):
        a=self.begin();b=self.j.transition(a.operation_id,a.revision,'RUNNING')
        self.assertEqual(b.revision,a.revision+1)
        with self.assertRaisesRegex(ValueError,'revision'):self.j.transition(a.operation_id,a.revision,'RUNNING')
    def test_illegal_skip_rejected(self):
        a=self.begin()
        with self.assertRaises(ValueError):self.j.transition(a.operation_id,a.revision,'SUCCEEDED')
    def test_success_requires_result(self):
        a=self.begin();a=self.j.transition(a.operation_id,a.revision,'RUNNING')
        with self.assertRaises(ValueError):self.j.transition(a.operation_id,a.revision,'SUCCEEDED')
    def test_result_survives_reopen(self):
        from lightsandbox_seed.journal import Journal
        a=self.begin();a=self.j.transition(a.operation_id,a.revision,'RUNNING');self.j.transition(a.operation_id,a.revision,'SUCCEEDED',{'return_code':0})
        self.j.close();self.j=Journal(self.path);self.assertEqual(self.begin().result,{'return_code':0})
    def test_running_after_crash_becomes_unknown_not_prepared(self):
        a=self.begin();self.j.transition(a.operation_id,a.revision,'RUNNING');self.j.reconcile_lost_processes()
        b=self.begin();self.assertEqual(b.status,'UNKNOWN');self.assertFalse(b.new)
    def test_unknown_not_automatically_retryable(self):
        a=self.begin();a=self.j.transition(a.operation_id,a.revision,'RUNNING');a=self.j.transition(a.operation_id,a.revision,'UNKNOWN')
        with self.assertRaises(ValueError):self.j.transition(a.operation_id,a.revision,'RUNNING')
    def test_prepared_survives_reconcile(self):
        a=self.begin();self.j.reconcile_lost_processes();self.assertEqual(self.begin().status,'PREPARED')
    def test_payload_is_hashed_not_stored(self):
        self.begin(body={'token':'SECRET_123'});self.j.close()
        self.assertNotIn(b'SECRET_123',self.path.read_bytes())
    def test_invalid_json_numbers_rejected(self):
        with self.assertRaises(ValueError):self.begin(body={'x':float('nan')})
