"""A deliberately small YAML subset reader (stdlib has no YAML).

Scope: block mappings/sequences, inline flow `[...]` / `{...}` collections,
single/double quoted scalars, plain scalars with JSON-ish typing, `#`
comments, and literal/folded blocks (`|`, `>`). That covers the compose and
task-config files used by the inventoried benchmarks. Anything outside the
subset raises MiniYamlError so callers can record the file unparsed instead
of silently guessing.
"""
from __future__ import annotations

import re

_INT = re.compile(r'^[-+]?\d+$')
_FLOAT = re.compile(r'^[-+]?(\d+\.\d*|\.\d+)([eE][-+]?\d+)?$')
_BLOCK_KEY = re.compile(r'^([A-Za-z0-9_.\-/]+):(?:[ \t]+(.*))?$')


class MiniYamlError(ValueError):
    pass


def _strip_comment(text: str) -> str:
    quote = None
    for i, ch in enumerate(text):
        if quote:
            if ch == quote and (quote == "'" or text[i - 1] != '\\'):
                quote = None
        elif ch in ('"', "'"):
            quote = ch
        elif ch == '#' and (i == 0 or text[i - 1] in ' \t'):
            return text[:i].rstrip()
    return text.rstrip()


def _prepared_lines(text: str):
    out = []
    for raw in text.splitlines():
        if '\t' in raw[: len(raw) - len(raw.lstrip())]:
            raise MiniYamlError('tab indentation is outside the supported subset')
        body = _strip_comment(raw)
        if body.strip():
            out.append((len(body) - len(body.lstrip(' ')), body.strip()))
    return out


def _parse_scalar(token: str):
    token = token.strip()
    if token == '':
        return None
    if token[0] == '"':
        if len(token) < 2 or token[-1] != '"':
            raise MiniYamlError(f'unbalanced double quote: {token!r}')
        body = token[1:-1]
        return (body.replace('\\\\', '\\').replace('\\"', '"').replace('\\n', '\n')
                .replace('\\t', '\t'))
    if token[0] == "'":
        if len(token) < 2 or token[-1] != "'":
            raise MiniYamlError(f'unbalanced single quote: {token!r}')
        return token[1:-1].replace("''", "'")
    low = token.lower()
    if low in ('null', '~'):
        return None
    if low == 'true':
        return True
    if low == 'false':
        return False
    if _INT.match(token):
        return int(token)
    if _FLOAT.match(token):
        return float(token)
    return token


def _split_flow(body: str):
    parts, depth, quote, start = [], 0, None, 0
    for i, ch in enumerate(body):
        if quote:
            if ch == quote and (quote == "'" or body[i - 1] != '\\'):
                quote = None
        elif ch in ('"', "'"):
            quote = ch
        elif ch in '[{':
            depth += 1
        elif ch in ']}':
            depth -= 1
        elif ch == ',' and depth == 0:
            parts.append(body[start:i])
            start = i + 1
    parts.append(body[start:])
    return [p for p in (p.strip() for p in parts) if p != '']


def _flow_depth(body: str) -> int:
    depth, quote = 0, None
    prev = ''
    for ch in body:
        if quote:
            if ch == quote and (quote == "'" or prev != '\\'):
                quote = None
        elif ch in ('"', "'"):
            quote = ch
        elif ch in '[{':
            depth += 1
        elif ch in ']}':
            depth -= 1
        prev = ch
    return depth


def _parse_flow(token: str):
    token = token.strip()
    if token.startswith('['):
        if not token.endswith(']'):
            raise MiniYamlError(f'unbalanced flow sequence: {token!r}')
        inner = token[1:-1].strip()
        if not inner:
            return []
        return [_parse_value(p) for p in _split_flow(inner)]
    if token.startswith('{'):
        if not token.endswith('}'):
            raise MiniYamlError(f'unbalanced flow mapping: {token!r}')
        inner = token[1:-1].strip()
        if not inner:
            return {}
        out = {}
        for part in _split_flow(inner):
            key, sep, val = part.partition(':')
            if not sep:
                raise MiniYamlError(f'flow mapping entry lacks colon: {part!r}')
            out[key.strip().strip('"\'')] = _parse_value(val.strip())
        return out
    return _parse_scalar(token)


def _parse_value(token: str):
    token = token.strip()
    if token.startswith('[') or token.startswith('{'):
        return _parse_flow(token)
    return _parse_scalar(token)


def loads(text: str):
    lines = _prepared_lines(text)
    if not lines:
        return None
    value, cursor = _parse_block(lines, 0, lines[0][0])
    if cursor != len(lines):
        raise MiniYamlError(f'trailing content at line {cursor + 1}')
    return value


def _parse_block(lines, i, indent):
    if i >= len(lines):
        return None, i
    if lines[i][0] != indent:
        raise MiniYamlError(f'bad indentation at line {i + 1}')
    if lines[i][1].lstrip()[:1] in ('[', '{'):
        # Flow collection starting on its own line (may span several).
        token = lines[i][1]
        i += 1
        while _flow_depth(token) > 0 and i < len(lines):
            token = token + ' ' + lines[i][1]
            i += 1
        if _flow_depth(token) != 0:
            raise MiniYamlError(f'unbalanced flow collection near line {i}')
        return _parse_value(token), i
    if lines[i][1] == '-' or lines[i][1].startswith('- '):
        return _parse_sequence(lines, i, indent)
    return _parse_mapping(lines, i, indent)


def _parse_mapping(lines, i, indent):
    out = {}
    while i < len(lines) and lines[i][0] == indent:
        content = lines[i][1]
        match = _BLOCK_KEY.match(content)
        if not match:
            raise MiniYamlError(f'expected mapping key at line {i + 1}: {content!r}')
        key, inline = match.group(1), match.group(2)
        i += 1
        if inline in ('|', '|-', '|+', '>', '>-', '>+'):
            block_lines = []
            while i < len(lines) and lines[i][0] > indent:
                block_lines.append(lines[i][1])
                i += 1
            joiner = '\n' if inline.startswith('|') else ' '
            value = joiner.join(block_lines)
        elif inline is not None and inline != '':
            if inline.lstrip()[:1] in ('[', '{'):
                while _flow_depth(inline) > 0 and i < len(lines) and lines[i][0] > indent:
                    inline = inline + ' ' + lines[i][1]
                    i += 1
                if _flow_depth(inline) > 0:
                    raise MiniYamlError(f'unbalanced flow collection at line {i}: {inline!r}')
            value = _parse_value(inline)
        else:
            if i < len(lines) and lines[i][0] > indent:
                value, i = _parse_block(lines, i, lines[i][0])
            else:
                value = None
        out[key] = value
    return out, i


def _parse_sequence(lines, i, indent):
    out = []
    while i < len(lines) and lines[i][0] == indent:
        content = lines[i][1]
        if content == '-':
            item_text = None
        elif content.startswith('- '):
            item_text = content[2:]
        else:
            break
        i += 1
        if item_text is None:
            if i < len(lines) and lines[i][0] > indent:
                value, i = _parse_block(lines, i, lines[i][0])
            else:
                value = None
            out.append(value)
            continue
        key_match = _BLOCK_KEY.match(item_text)
        if key_match and not item_text.startswith(('"', "'")) and '\t' not in item_text:
            # Mapping entry inside a sequence: "- key: value" possibly followed
            # by further pairs at a deeper indent.
            key, inline = key_match.group(1), key_match.group(2)
            mapping = {}
            if inline:
                if inline.lstrip()[:1] in ('[', '{'):
                    while _flow_depth(inline) > 0 and i < len(lines) and lines[i][0] > indent:
                        inline = inline + ' ' + lines[i][1]
                        i += 1
                mapping[key] = _parse_value(inline)
            elif i < len(lines) and lines[i][0] > indent:
                mapping[key], i = _parse_block(lines, i, lines[i][0])
            else:
                mapping[key] = None
            if i < len(lines) and lines[i][0] > indent and not \
                    (lines[i][1] == '-' or lines[i][1].startswith('- ')):
                rest, i = _parse_mapping(lines, i, lines[i][0])
                mapping.update(rest)
            out.append(mapping)
            continue
        if not key_match and i < len(lines) and lines[i][0] > indent:
            # Plain scalar continuation, folded with a space.
            extra = []
            while i < len(lines) and lines[i][0] > indent:
                extra.append(lines[i][1])
                i += 1
            item_text = item_text + ' ' + ' '.join(extra)
        out.append(_parse_value(item_text))
    return out, i
