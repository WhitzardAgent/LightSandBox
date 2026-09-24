from pathlib import Path
from tempfile import TemporaryDirectory
import os, unittest

class BundleTests(unittest.TestCase):
    def setUp(self):self.tmp=TemporaryDirectory();self.root=Path(self.tmp.name)
    def tearDown(self):self.tmp.cleanup()
    def manifest(self,**kw):
        from lightsandbox_seed.bundle import snapshot_tree
        return snapshot_tree(self.root,**kw)
    def test_empty_directories_in_identity(self):
        a=self.manifest();(self.root/'empty').mkdir();b=self.manifest();self.assertNotEqual(a.digest,b.digest)
    def test_mode_is_in_identity(self):
        p=self.root/'run.sh';p.write_text('echo hi');p.chmod(0o644);a=self.manifest();p.chmod(0o755);self.assertNotEqual(a.digest,self.manifest().digest)
    def test_mtime_does_not_change_content_identity(self):
        p=self.root/'a';p.write_text('x');a=self.manifest();os.utime(p,(1,1));self.assertEqual(a.digest,self.manifest().digest)
    def test_content_change_changes_identity(self):
        p=self.root/'a';p.write_text('x');a=self.manifest();p.write_text('y');self.assertNotEqual(a.digest,self.manifest().digest)
    def test_paths_sorted(self):
        (self.root/'z').write_text('x');(self.root/'a').write_text('x');self.assertEqual([e.path for e in self.manifest().entries],['a','z'])
    def test_symlink_rejected_with_explicit_reason(self):
        (self.root/'link').symlink_to('/etc/passwd')
        with self.assertRaisesRegex(ValueError,'symlink'):self.manifest()
    def test_fifo_rejected_without_blocking(self):
        os.mkfifo(self.root/'fifo')
        with self.assertRaisesRegex(ValueError,'special'):self.manifest()
    def test_file_limit(self):
        (self.root/'a').write_text('too long')
        with self.assertRaises(ValueError):self.manifest(max_file_bytes=2)
    def test_total_limit(self):
        (self.root/'a').write_text('123');(self.root/'b').write_text('456')
        with self.assertRaises(ValueError):self.manifest(max_total_bytes=5)
    def test_entry_limit(self):
        (self.root/'a').write_text('x');(self.root/'b').write_text('x')
        with self.assertRaises(ValueError):self.manifest(max_entries=1)
    def test_relative_target_validation(self):
        from lightsandbox_seed.bundle import validate_relative_path
        for x in ['../a','/etc/passwd','a/../b','a//b','a\x00b','a\\b','.', '']:
            with self.subTest(x=x),self.assertRaises(ValueError):validate_relative_path(x)
        self.assertEqual(validate_relative_path('a/b.txt'),'a/b.txt')
    def test_archive_is_deterministic(self):
        from lightsandbox_seed.bundle import export_bundle
        (self.root/'a').write_text('x');a=export_bundle(self.root);b=export_bundle(self.root)
        self.assertEqual(a,b)
    def test_archive_contains_only_given_environment(self):
        import io,tarfile
        from lightsandbox_seed.bundle import export_bundle
        (self.root/'a').write_text('x')
        with tarfile.open(fileobj=io.BytesIO(export_bundle(self.root)),mode='r:') as tf:
            self.assertEqual(tf.getnames(),['a'])
