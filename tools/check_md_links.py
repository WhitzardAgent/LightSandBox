#!/usr/bin/env python3
"""Documentation guards: Markdown link resolution and bash-snippet allowlist.

Link mode (default): walks docs/**/*.md plus the root README.md, resolves
every relative Markdown link against the file's directory, and fails on
missing targets. http(s)/mailto URLs and #-only anchors are ignored; anchors
on file targets are stripped before resolution.

Snippet mode (--snippets): extracts fenced ```bash blocks from the same files
and requires every command line to start from the allowlist of repo-real
entry points — checked-in tools/*.py and dev/ scripts, dev/check itself,
`python3 <repo script>` (the named file must exist), cargo build/test
commands, mkdocs, and ordinary environment setup verbs — so docs never teach
invented binaries. Exit 1 on any violation or on zero collected inputs.

Exit codes: 0 clean, 1 findings or zero collected, 2 usage.
"""
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]

LINK_RE = re.compile(r'(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)')
FENCE_RE = re.compile(r'^```(bash|sh|shell)[^\n]*\n(.*?)^```', re.M | re.S)

ENV_VERBS = {'cd', 'source', '.', 'export', 'set', 'unset', 'git', 'rustup',
             'pip', 'pip3', 'mkdir', 'echo', 'printf', 'which', 'lsbx', 'harbor', 'mkdocs'}
CARGO_SUBCOMMANDS = {'test', 'build', 'check', 'run', 'fmt', 'clippy'}
PYTHON_RE = re.compile(r'python(?:3(?:\.\d{1,2})?)?')
REPO_SCRIPT_RE = re.compile(r'(\./)?(tools|dev|tests|examples|client|seed|spike)/[\w./-]+')
ASSIGN_RE = re.compile(r'[A-Za-z_][A-Za-z0-9_]*=\S+')


def target_files():
    files = sorted((ROOT / 'docs').rglob('*.md'))
    readme = ROOT / 'README.md'
    if readme.is_file():
        files.append(readme)
    return files


def check_links(files):
    total = 0
    broken = []
    for path in files:
        text = path.read_text(encoding='utf-8', errors='replace')
        for match in LINK_RE.finditer(text):
            target = match.group(1)
            url = urlsplit(target)
            if url.scheme or url.netloc:
                continue
            rel_target = unquote(url.path)
            if not rel_target:
                continue
            total += 1
            resolved = (path.parent / rel_target).resolve()
            if not resolved.is_relative_to(ROOT) or not resolved.exists():
                line = text.count('\n', 0, match.start()) + 1
                broken.append((path.relative_to(ROOT), line, target))
    for path, line, target in broken:
        print(f'{path}:{line}: missing link target: {target}')
    print(f'checked {total} links in {len(files)} markdown files; {len(broken)} broken')
    return 1 if broken or not files else 0


def _split_top_level(line, separator):
    """Split on a shell operator that appears outside single/double quotes."""
    parts = []
    quote = None
    current = []
    i = 0
    while i < len(line):
        char = line[i]
        if quote:
            if char == quote:
                quote = None
        elif char in ('\'', '"'):
            quote = char
        elif line.startswith(separator, i):
            parts.append(''.join(current))
            current = []
            i += len(separator)
            continue
        current.append(char)
        i += 1
    parts.append(''.join(current))
    return parts


def _command_allowed(line, path, lineno):
    """True when the command line's entry point is repo-real or environment tooling."""
    token = line.split()[0]
    if token in ('./dev/check', 'dev/check'):
        return True
    if token in ('cargo', 'rustc'):
        parts = line.split()
        return token == 'rustc' or (len(parts) > 1 and parts[1] in CARGO_SUBCOMMANDS)
    if token in ENV_VERBS:
        return True
    if PYTHON_RE.fullmatch(token):
        parts = line.split()
        if len(parts) < 2:
            return False
        second = parts[1]
        if second.startswith('-'):
            return True
        return _repo_path_ok(second, path, lineno)
    if REPO_SCRIPT_RE.fullmatch(token):
        return _repo_path_ok(token, path, lineno)
    return False


def _repo_path_ok(token, path, lineno):
    candidate = (ROOT / token.lstrip('./')) if not token.startswith('./') else (ROOT / token[2:])
    try:
        ok = candidate.resolve().is_relative_to(ROOT) and candidate.is_file()
    except OSError:
        ok = False
    return ok


def check_snippets(files):
    blocks = 0
    violations = []
    for path in files:
        text = path.read_text(encoding='utf-8', errors='replace')
        for fence in FENCE_RE.finditer(text):
            blocks += 1
            base_line = text.count('\n', 0, fence.start(2)) + 1
            continuation = False
            for offset, raw in enumerate(fence.group(2).splitlines()):
                line = raw.strip()
                lineno = base_line + offset
                if continuation:
                    continuation = raw.rstrip().endswith('\\')
                    continue
                continuation = raw.rstrip().endswith('\\')
                if line.startswith('$'):
                    line = line[1:].strip()
                if not line or line.startswith('#'):
                    continue
                while True:
                    assign = ASSIGN_RE.match(line)
                    if assign and line[assign.end():].startswith(' '):
                        line = line[assign.end():].strip()
                    else:
                        break
                for segment in _split_top_level(line, '&&'):
                    segment = segment.strip()
                    if not segment or segment.startswith('#'):
                        continue
                    if not _command_allowed(segment, path, lineno):
                        violations.append((path.relative_to(ROOT), lineno, segment))
    for path, lineno, segment in violations:
        print(f'{path}:{lineno}: non-allowlisted command: {segment}')
    print(f'checked {blocks} bash blocks in {len(files)} markdown files; '
          f'{len(violations)} non-allowlisted commands')
    return 1 if violations or blocks == 0 else 0


def main(argv):
    args = [a for a in argv if not a.startswith('-') or a == '--snippets']
    if set(a for a in argv if a.startswith('-')) - {'--snippets'}:
        print(__doc__, file=sys.stderr)
        return 2
    files = [f for f in target_files() if f.is_file()]
    if not files:
        print('zero markdown files collected under docs/ + README.md')
        return 1
    if '--snippets' in argv:
        return check_snippets(files)
    return check_links(files)


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
