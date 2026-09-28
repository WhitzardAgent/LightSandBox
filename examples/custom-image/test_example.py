import hashlib
import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
IMAGE_DIR = HERE / 'image'


class TestCustomImageExample(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.proc = subprocess.run([sys.executable, str(HERE / 'plan_image.py')],
                                  cwd=str(ROOT), capture_output=True, text=True, timeout=120)

    def test_exits_zero(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)

    def test_digest_chain_and_pin_output(self):
        out = self.proc.stdout
        index = json.loads((IMAGE_DIR / 'index.json').read_text(encoding='utf-8'))
        digest = index['manifests'][0]['digest']
        self.assertIn(f'manifest verified: {digest}', out)
        self.assertIn('pin-by-digest reference: registry.example.invalid/custom-demo@' + digest, out)
        line = [l for l in out.splitlines() if l.startswith('{') and '"method"' in l][0]
        request = json.loads(line)
        self.assertEqual(request['path'], '/v1/sandboxes')
        self.assertEqual(request['body']['image'],
                         'registry.example.invalid/custom-demo@' + digest)
        self.assertFalse(request['executed'])
        self.assertIn('planning-only', out)

    def test_checked_in_blobs_are_content_addressed(self):
        blobs = list((IMAGE_DIR / 'blobs' / 'sha256').iterdir())
        self.assertGreaterEqual(len(blobs), 3, 'index, config and layer blobs must exist')
        for blob in blobs:
            self.assertEqual(hashlib.sha256(blob.read_bytes()).hexdigest(), blob.name)

    def test_layout_version_and_layer_is_empty_tar(self):
        layout = json.loads((IMAGE_DIR / 'oci-layout').read_text(encoding='utf-8'))
        self.assertEqual(layout['imageLayoutVersion'], '1.0.0')
        index = json.loads((IMAGE_DIR / 'index.json').read_text(encoding='utf-8'))
        manifest = json.loads((IMAGE_DIR / 'blobs' / 'sha256'
                               / index['manifests'][0]['digest'].split(':', 1)[1]).read_bytes())
        layer = manifest['layers'][0]
        data = (IMAGE_DIR / 'blobs' / 'sha256' / layer['digest'].split(':', 1)[1]).read_bytes()
        self.assertEqual(len(data), 10240)
        self.assertEqual(set(data), {0}, 'the fixture layer is a valid empty tar (zero blocks)')


if __name__ == '__main__':
    unittest.main()
