import unittest
class EventTests(unittest.TestCase):
    def make(self,**kw):
        from lightsandbox_seed.events import ExecAccumulator
        return ExecAccumulator('run1',generation=1,**kw)
    def data(self,seq=1,stream='stdout',raw=b'hello'):
        import base64
        return dict(run_id='run1',generation=1,sequence=seq,kind='output',stream=stream,data=base64.b64encode(raw).decode())
    def exited(self,seq=2,code=0):return dict(run_id='run1',generation=1,sequence=seq,kind='exit',return_code=code)
    def test_collects_stdout_and_exit(self):
        a=self.make();a.accept(self.data());a.accept(self.exited());self.assertEqual(a.result().stdout,b'hello')
    def test_zero_code_not_none(self):
        a=self.make();a.accept(self.exited(seq=1));self.assertEqual(a.result().return_code,0)
    def test_missing_exit_not_success(self):
        a=self.make();a.accept(self.data())
        with self.assertRaisesRegex(ValueError,'incomplete'):a.result()
    def test_sequence_gap(self):
        a=self.make()
        with self.assertRaisesRegex(ValueError,'sequence'):a.accept(self.data(seq=2))
    def test_replay_duplicate_does_not_append(self):
        a=self.make();a.accept(self.data());a.accept(self.data());a.accept(self.exited());self.assertEqual(a.result().stdout,b'hello')
    def test_duplicate_with_changed_payload_rejected(self):
        a=self.make();a.accept(self.data())
        with self.assertRaises(ValueError):a.accept(self.data(raw=b'changed'))
    def test_stale_generation(self):
        a=self.make();e=self.data();e['generation']=2
        with self.assertRaisesRegex(ValueError,'generation'):a.accept(e)
    def test_wrong_identity(self):
        a=self.make();e=self.data();e['run_id']='other'
        with self.assertRaises(ValueError):a.accept(e)
    def test_unknown_kind_rejected(self):
        a=self.make();e=self.data();e['kind']='success-ish'
        with self.assertRaises(ValueError):a.accept(e)
    def test_output_after_exit_rejected(self):
        a=self.make();a.accept(self.exited(seq=1))
        with self.assertRaises(ValueError):a.accept(self.data(seq=2))
    def test_split_utf8_retained_as_bytes(self):
        a=self.make();b='沙箱'.encode();a.accept(self.data(raw=b[:2]));a.accept(self.data(seq=2,raw=b[2:]));a.accept(self.exited(seq=3));self.assertEqual(a.result().stdout,b)
    def test_stderr_separate(self):
        a=self.make();a.accept(self.data(stream='stderr'));a.accept(self.exited(code=3));r=a.result();self.assertEqual(r.stdout,b'');self.assertEqual(r.stderr,b'hello');self.assertEqual(r.return_code,3)
    def test_output_budget_no_silent_truncation(self):
        a=self.make(max_output_bytes=2)
        with self.assertRaisesRegex(ValueError,'budget'):a.accept(self.data())
    def test_bool_sequence_rejected(self):
        a=self.make()
        with self.assertRaises(ValueError):a.accept(self.data(seq=True))
