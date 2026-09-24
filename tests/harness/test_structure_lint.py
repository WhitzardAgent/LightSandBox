"""T00-C03: structure lint must catch dependency-direction violations,
forbidden domain vocabulary in runtime crates, and verdict-weakening
skip markers — by exact path, on a modified temporary copy.
"""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'dev'))

import checklib  # noqa: E402


def make_workspace_copy(dst: Path) -> None:
    """Copy only the crate structure (not build artifacts) into a temp root."""
    for item in ('Cargo.toml', 'Cargo.lock'):
        src = ROOT / item
        if src.exists():
            shutil.copy2(src, dst / item)
    for crate in (ROOT / 'crates').iterdir():
        if not crate.is_dir():
            continue
        target = dst / 'crates' / crate.name
        (target / 'src').mkdir(parents=True)
        shutil.copy2(crate / 'Cargo.toml', target / 'Cargo.toml')
        for src in (crate / 'src').glob('*.rs'):
            shutil.copy2(src, target / 'src' / src.name)


class StructureLintTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        make_workspace_copy(self.root)

    def tearDown(self):
        self.tmp.cleanup()

    def lint(self):
        return checklib.lint_structure(self.root)

    def test_clean_tree_has_no_findings(self):
        findings = self.lint()
        self.assertEqual(findings, [], findings)

    def test_reverse_dependency_is_flagged_by_path(self):
        """T00-C03: injecting a core->conformance edge must fail with the exact path."""
        manifest = self.root / 'crates' / 'ls-core' / 'Cargo.toml'
        manifest.write_text(manifest.read_text() + '\n[dev-dependencies]\nconformance = { path = "../conformance" }\n')
        findings = self.lint()
        self.assertTrue(findings, 'lint accepted a reverse dependency edge')
        joined = ' '.join(f"[{f['rule']}] {f['path']}: {f['detail']}" for f in findings)
        self.assertIn('crates/ls-core/Cargo.toml', joined, joined)
        self.assertIn('conformance', joined, joined)

    def test_forbidden_vocabulary_is_flagged_by_path(self):
        target = self.root / 'crates' / 'ls-guest' / 'src' / 'lib.rs'
        target.write_text(target.read_text() + '\nstruct Evaluation;\n')
        findings = self.lint()
        joined = ' '.join(f"[{f['rule']}] {f['path']}: {f['detail']}" for f in findings)
        self.assertIn('forbidden-vocabulary', joined, joined)
        self.assertIn('ls-guest', joined, joined)

    def test_ignored_rust_test_is_verdict_weakening(self):
        target = self.root / 'crates' / 'ls-host' / 'src' / 'lib.rs'
        target.write_text(target.read_text() + '\n#[test]\n#[ignore]\nfn skipped() {}\n')
        findings = self.lint()
        joined = ' '.join(f"[{f['rule']}] {f['path']}: {f['detail']}" for f in findings)
        self.assertIn('verdict-protection', joined, joined)

    def test_unknown_workspace_member_is_flagged(self):
        rogue = self.root / 'crates' / 'conformance'
        (rogue / 'src').mkdir(parents=True)
        (rogue / 'src' / 'lib.rs').write_text('pub fn x() {}\n')
        (rogue / 'Cargo.toml').write_text('[package]\nname = "conformance"\nversion = "0.0.0"\nedition = "2021"\n')
        findings = self.lint()
        joined = ' '.join(f"[{f['rule']}] {f['path']}: {f['detail']}" for f in findings)
        self.assertIn('unknown-crate', joined, joined)


if __name__ == '__main__':
    unittest.main()
