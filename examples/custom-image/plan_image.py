#!/usr/bin/env python3
"""custom-image: verify a prebuilt OCI image layout and plan pin-by-digest use.

Reads the checked-in OCI image layout under image/ and re-verifies the whole
content-addressed chain — index.json -> manifest -> config -> layer blobs —
by recomputing every sha256. Then shows the digest-pinned image reference a
`sandbox create` request would carry, in --dry-run form. No registry, Docker,
or BuildKit is involved; this example never builds or pulls anything.
"""
import hashlib
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
IMAGE_DIR = Path(__file__).resolve().parent / 'image'


def read_blob(digest):
    algo, _, hexpart = digest.partition(':')
    if algo != 'sha256' or not hexpart:
        raise SystemExit(f'unsupported digest reference: {digest!r}')
    data = (IMAGE_DIR / 'blobs' / algo / hexpart).read_bytes()
    actual = hashlib.sha256(data).hexdigest()
    if actual != hexpart:
        raise SystemExit(f'blob digest mismatch: expected {digest}, recomputed sha256:{actual}')
    return data


def main():
    layout = json.loads((IMAGE_DIR / 'oci-layout').read_text(encoding='utf-8'))
    if layout.get('imageLayoutVersion') != '1.0.0':
        raise SystemExit(f'unknown OCI layout version: {layout!r}')

    index = json.loads((IMAGE_DIR / 'index.json').read_text(encoding='utf-8'))
    manifests = index.get('manifests') or []
    if len(manifests) != 1:
        raise SystemExit(f'fixture expects exactly one manifest descriptor, got {len(manifests)}')

    descriptor = manifests[0]
    manifest_bytes = read_blob(descriptor['digest'])
    if len(manifest_bytes) != descriptor['size']:
        raise SystemExit('manifest size does not match its descriptor')
    manifest = json.loads(manifest_bytes)
    print('[1/3] manifest verified:', descriptor['digest'])

    config = json.loads(read_blob(manifest['config']['digest']))
    print('[2/3] config verified:', manifest['config']['digest'],
          f"(os={config['os']}, arch={config['architecture']})")

    diff_ids = config['rootfs']['diff_ids']
    if len(manifest['layers']) != len(diff_ids):
        raise SystemExit('layer count and diff_id count diverge')
    for layer, diff_id in zip(manifest['layers'], diff_ids):
        data = read_blob(layer['digest'])
        if len(data) != layer['size']:
            raise SystemExit('layer size does not match its descriptor')
        if layer['mediaType'] == 'application/vnd.oci.image.layer.v1.tar':
            expected = layer['digest']
        else:
            expected = 'sha256:' + hashlib.sha256(data).hexdigest()
        if diff_id != expected:
            raise SystemExit(f'diff_id {diff_id} does not match layer content {expected}')
    print('[3/3] layers verified:', [l['digest'] for l in manifest['layers']])

    ref = 'registry.example.invalid/custom-demo@' + descriptor['digest']
    print('digest chain OK -> pin-by-digest reference:', ref)

    proc = subprocess.run([sys.executable, str(ROOT / 'tools' / 'lsbx.py'), '--json',
                           'sandbox', 'create', '--image', ref, '--dry-run'],
                          capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        raise SystemExit(f'lsbx create dry-run failed: {proc.stderr.strip()}')
    print(json.dumps(json.loads(proc.stdout)))
    if json.loads(proc.stdout)['executed'] is not False:
        raise SystemExit('a dry run reported executed=true; refusing to present a fake run')
    print('planning-only: the image is verified content, the create is a request preview')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
