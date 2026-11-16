"""The string literals the Words test classifies (experimental/rex/UI.md, Words).

Prints one JSON document: every literal outside test code in src/ (Rust) and
scripts/ (Python and sh) that holds a host word, every Rex text that a
`kind.text(herdr, rex)` or `.replace(herdr, rex)` pair puts in
place of a Herdr text, and every format string that names its host through
`kind.title()`. Each literal carries its file, line and decoded text, and a
format string also its fixed fragments, which a Rex output must hold in order.

`--relocate ALLOWLIST` rewrites the line of each allowlist entry whose literal
moved within its file, and touches nothing else: a new literal still needs a
decision.
"""
import ast
import json
from pathlib import Path
import re
import sys

WORDS = re.compile(r'herdr|popup|plugin|\bprefix\b|rex\s+(?:space|workspace)', re.I)
ROOT = Path(__file__).resolve().parents[1]


def rust_tokens(text):
    """(kind, value, line) for the significant tokens of Rust source: str
    (decoded), ident, and punct. Comments and char literals are dropped."""
    i, n, line = 0, len(text), 1
    while i < n:
        c = text[i]
        if c == '\n':
            line += 1
            i += 1
        elif c in ' \t\r':
            i += 1
        elif text.startswith('//', i):
            j = text.find('\n', i)
            i = n if j < 0 else j
        elif text.startswith('/*', i):
            depth, j = 1, i + 2
            while depth and j < n:
                if text.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif text.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            line += text.count('\n', i, j)
            i = j
        elif (m := re.match(r'(b?r)(#*)"', text[i:i + 80])) and not (i and (text[i - 1].isalnum() or text[i - 1] == '_')):
            hashes = m.group(2)
            start = i + len(m.group(0))
            end = text.index('"' + hashes, start)
            yield 'str', text[start:end], line
            line += text.count('\n', i, end)
            i = end + 1 + len(hashes)
        elif c == '"' or (c == 'b' and text.startswith('b"', i) and not (i and (text[i - 1].isalnum() or text[i - 1] == '_'))):
            j = i + (2 if c == 'b' else 1)
            out = []
            while text[j] != '"':
                if text[j] == '\\':
                    nxt = text[j + 1]
                    if nxt == '\n':
                        j += 2
                        while text[j] in ' \t\n\r':
                            j += 1
                        continue
                    if nxt == 'u':
                        close = text.index('}', j)
                        out.append(chr(int(text[j + 3:close], 16)))
                        j = close + 1
                        continue
                    if nxt == 'x':
                        out.append(chr(int(text[j + 2:j + 4], 16)))
                        j += 4
                        continue
                    out.append({'n': '\n', 't': '\t', 'r': '\r', '0': '\0'}.get(nxt, nxt))
                    j += 2
                else:
                    out.append(text[j])
                    j += 1
            yield 'str', ''.join(out), line
            line += text.count('\n', i, j)
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'\n])'", text[i:i + 12])
            i += len(m.group(0)) if m else 1
        elif c.isalnum() or c == '_':
            m = re.match(r'\w+', text[i:])
            yield 'ident', m.group(0), line
            i += len(m.group(0))
        else:
            yield 'punct', c, line
            i += 1


def without_test_items(tokens):
    """The tokens outside `#[cfg(test)]` and test-support items, and the
    module files those items declare."""
    out, skipped_modules, i = [], [], 0
    while i < len(tokens):
        kind, value, _ = tokens[i]
        if kind == 'punct' and value == '#' and i + 1 < len(tokens) and tokens[i + 1][1] == '[':
            depth, j = 0, i + 1
            while True:
                if tokens[j][1] == '[':
                    depth += 1
                elif tokens[j][1] == ']':
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            attribute = ' '.join(str(t[1]) for t in tokens[i + 2:j])
            if attribute in ('cfg ( test )', 'cfg ( feature = test-support )', 'cfg ( any ( test , feature = test-support ) )', 'cfg ( all ( test , feature = test-support ) )'):
                k, item = j + 1, []
                while tokens[k][1] not in ('{', ';'):
                    item.append(tokens[k])
                    k += 1
                if tokens[k][1] == ';':
                    words = [t[1] for t in item]
                    path = [t[1] for t in item if t[0] == 'str']
                    if 'mod' in words:
                        skipped_modules.append((path[0] if path else words[-1] + '.rs', bool(path)))
                    i = k + 1
                    continue
                depth = 0
                while True:
                    if tokens[k][1] == '{':
                        depth += 1
                    elif tokens[k][1] == '}':
                        depth -= 1
                        if depth == 0:
                            break
                    k += 1
                i = k + 1
                continue
        out.append(tokens[i])
        i += 1
    return out, skipped_modules


def fragments(template, python=False):
    """The fixed parts of a format string, in order."""
    parts, current, i = [], [], 0
    while i < len(template):
        c = template[i]
        if c in '{}' and template[i + 1:i + 2] == c:
            current.append(c)
            i += 2
        elif c == '{':
            parts.append(''.join(current))
            current = []
            depth = 1
            i += 1
            while depth:
                depth += {'{': 1, '}': -1}.get(template[i], 0)
                i += 1
        else:
            current.append(c)
            i += 1
    parts.append(''.join(current))
    return [part for part in parts if part]


def rust_literals(path, text):
    tokens, modules = without_test_items(list(rust_tokens(text)))
    consts = {}
    for i in range(len(tokens) - 6):
        if tokens[i][1] == 'const' and tokens[i + 2][1] == ':' and tokens[i + 1][0] == 'ident':
            j = i + 3
            while j < len(tokens) and tokens[j][1] != '=':
                j += 1
            if j + 1 < len(tokens) and tokens[j + 1][0] == 'str' and tokens[j + 2][1] == ';':
                consts[tokens[i + 1][1]] = (tokens[j + 1][1], tokens[j + 1][2])
    found = {}

    def add(index, role, **extra):
        kind, value, line = tokens[index]
        key = (line, value, index)
        found.setdefault(key, {'line': line, 'text': value, 'role': 'plain'})
        if role != 'plain':
            found[key]['role'] = role
            found[key].update(extra)

    def literal(index):
        return tokens[index][0] == 'str' if index < len(tokens) else False

    for i, (kind, value, line) in enumerate(tokens):
        if kind == 'str' and WORDS.search(value):
            add(i, 'plain')
        if kind == 'punct' and value == '.' and i + 2 < len(tokens) and tokens[i + 2][1] == '(':
            name = tokens[i + 1][1]
            a, comma, b = i + 3, i + 4, i + 5
            if name == 'text' and tokens[comma][1] == ',' and b < len(tokens):
                if literal(a) and literal(b):
                    add(a, 'herdr-arm', rex=tokens[b][1], rex_line=tokens[b][2])
                    add(b, 'rex-arm')
                elif tokens[a][0] == 'ident' and tokens[b][0] == 'ident' and tokens[a][1] in consts and tokens[b][1] in consts:
                    (herdr, herdr_line), (rex, rex_line) = consts[tokens[a][1]], consts[tokens[b][1]]
                    for key, item in list(found.items()):
                        if item['line'] == herdr_line and item['text'] == herdr:
                            item.update(role='herdr-arm', rex=rex, rex_line=rex_line)
                    found[(rex_line, rex, -1)] = {'line': rex_line, 'text': rex, 'role': 'rex-arm'}
            elif name == 'replace' and literal(a) and tokens[comma][1] == ',' and literal(b) and WORDS.search(tokens[a][1]):
                add(a, 'herdr-arm', rex=tokens[b][1], rex_line=tokens[b][2])
                add(b, 'rex-arm')
            elif name == 'title' and tokens[i + 3][1] == ')':
                depth, k = 0, i
                while k > 0:
                    k -= 1
                    if tokens[k][1] == ')':
                        depth += 1
                    elif tokens[k][1] == '(':
                        if depth == 0 and tokens[k - 1][1] == '!':
                            macro = tokens[k - 2][1]
                            first = k + 1
                            if macro in ('write', 'writeln'):
                                while tokens[first][1] != ',':
                                    first += 1
                                first += 1
                            assert literal(first), (path, line, 'a title() outside a format string')
                            add(first, 'title-format', fragments=fragments(tokens[first][1]))
                            break
                        depth -= 1
    for item in found.values():
        if item['role'] == 'plain' and '{' in item['text']:
            item['fragments'] = fragments(item['text'])
    return sorted(found.values(), key=lambda item: (item['line'], item['text'])), modules


def python_literals(text, first_line=1):
    """Plain literals of Python source; an f-string is a format string."""
    tree = ast.parse(text)
    docstrings = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            if node.body and isinstance(node.body[0], ast.Expr):
                value = node.body[0].value
                if isinstance(value, ast.Constant) and isinstance(value.value, str):
                    docstrings.add((value.lineno + first_line - 1, value.col_offset))
    found, i, n, line = [], 0, len(text), first_line
    while i < n:
        c = text[i]
        if c == '\n':
            line += 1
            i += 1
        elif c == '#':
            j = text.find('\n', i)
            i = n if j < 0 else j
        elif (m := re.match(r'([rRbBuUfF]{0,2})(\'\'\'|"""|\'|")', text[i:i + 6])) and not (i and (text[i - 1].isalnum() or text[i - 1] == '_')):
            prefix, quote = m.group(1).lower(), m.group(2)
            j, out = i + len(m.group(0)), []
            while not text.startswith(quote, j):
                if text[j] == '\\' and 'r' not in prefix:
                    nxt = text[j + 1]
                    if nxt == '\n':
                        j += 2
                        continue
                    out.append({'n': '\n', 't': '\t', 'r': '\r', '0': '\0'}.get(nxt, nxt if nxt in '\'"\\' else '\\' + nxt))
                    j += 2
                else:
                    out.append(text[j])
                    j += 1
            value = ''.join(out)
            column = len(text[text.rfind('\n', 0, i) + 1:i].encode('utf-8'))
            if WORDS.search(value) and (line, column) not in docstrings:
                item = {'line': line, 'text': value, 'role': 'plain'}
                if 'f' in prefix:
                    item['fragments'] = fragments(value)
                found.append(item)
            line += text.count('\n', i, j)
            i = j + len(quote)
        elif c.isalnum() or c == '_':
            m = re.match(r'\w+', text[i:])
            i += len(m.group(0))
        else:
            i += 1
    return found


def shell_literals(text):
    """Shell words and heredoc text; Python-fed heredocs use Python syntax."""
    found, i, n, line, heredocs = [], 0, len(text), 1, []
    while i < n:
        c = text[i]
        if c == '\n':
            line += 1
            i += 1
            if heredocs:
                for delimiter, strip_tabs, python in heredocs:
                    end = re.search(r'^' + (r'\t*' if strip_tabs else '') + re.escape(delimiter) + r'\r?$', text[i:], re.M)
                    if end is None:
                        raise ValueError(f'unterminated heredoc {delimiter!r}')
                    body = text[i:i + end.start()]
                    if python:
                        found += python_literals(body, line)
                    else:
                        for offset, value in enumerate(body.splitlines()):
                            if WORDS.search(value):
                                found.append({'line': line + offset, 'text': value, 'role': 'plain'})
                    consumed = end.end()
                    line += text.count('\n', i, i + consumed)
                    i += consumed
                    if text[i:i + 1] == '\n':
                        i += 1
                        line += 1
                heredocs.clear()
        elif c == '#' and (i == 0 or text[i - 1] in ' \t\n;'):
            j = text.find('\n', i)
            i = n if j < 0 else j
        elif (m := re.match(r"<<(-?)[ \t]*(['\"]?)(\w+)\2", text[i:])):
            command = text[text.rfind('\n', 0, i) + 1:i].lstrip()
            heredocs.append((m.group(3), bool(m.group(1)), bool(re.match(r'python[0-9.]*\s', command))))
            i += len(m.group(0))
        elif c == '\\':
            line += text[i + 1:i + 2] == '\n'
            i += 2
        elif c in '\'"':
            j, out = i + 1, []
            while text[j] != c:
                if c == '"' and text[j] == '\\' and text[j + 1] in '"\\$`':
                    out.append(text[j + 1])
                    j += 2
                else:
                    out.append(text[j])
                    j += 1
            value = ''.join(out)
            if WORDS.search(value):
                item = {'line': line, 'text': value, 'role': 'plain'}
                if c == '"' and '$' in value:
                    item['fragments'] = [part for part in re.split(r'\$\{?\w+\}?|\$\(.*?\)', value) if part]
                found.append(item)
            line += text.count('\n', i, j)
            i = j + 1
        elif c not in ' \t\r;|&()<>':
            start, out = i, []
            while i < n and text[i] not in "\r\n;|&()<>'\"":
                if text[i] == '\\' and i + 1 < n:
                    i += 1
                out.append(text[i])
                i += 1
            value = ''.join(out).strip()
            if WORDS.search(value):
                found.append({'line': line, 'text': value, 'role': 'plain'})
            if i == start:
                i += 1
        else:
            i += 1
    return found


def scan(root):
    literals, parsed, skipped = [], {}, set()
    for path in sorted((root / 'src').rglob('*.rs')):
        parsed[path], modules = rust_literals(path.relative_to(root).as_posix(), path.read_text())
        directory = path.parent if path.stem in ('lib', 'main', 'mod') else path.parent / path.stem
        for module, explicit_path in modules:
            name = Path(module).stem
            candidates = [path.parent / module] if explicit_path else [directory / f'{name}.rs', directory / name / 'mod.rs']
            skipped.update(candidate.resolve() for candidate in candidates)
            if not explicit_path:
                skipped.update(item.resolve() for item in (directory / name).rglob('*.rs'))
    for path, items in parsed.items():
        if path.resolve() not in skipped:
            literals += [dict(item, file=path.relative_to(root).as_posix()) for item in items]
    for path in sorted((root / 'scripts').iterdir()):
        relative = path.relative_to(root).as_posix()
        if path.suffix == '.py':
            literals += [dict(item, file=relative) for item in python_literals(path.read_text())]
        elif path.suffix == '.sh':
            literals += [dict(item, file=relative) for item in shell_literals(path.read_text())]
    path = root / 'src/rex_ui/module.lua'
    if path.exists():
        pattern = re.compile(r"--\[(=*)\[.*?\]\1\]|--[^\n]*|\[(=*)\[(.*?)\]\2\]|(['\"])((?:\\.|(?!\4).)*?)\4", re.S)
        text = path.read_text()
        for match in pattern.finditer(text):
            if match.group(0).startswith('--'):
                continue
            value = match.group(3) if match.group(2) is not None else match.group(5)
            if WORDS.search(value):
                literals.append({'file': path.relative_to(root).as_posix(), 'line': text.count('\n', 0, match.start()) + 1, 'text': value, 'role': 'plain'})
    return literals


def relocate(allowlist_path, literals):
    entries = json.loads(allowlist_path.read_text())
    found = {}
    for literal in literals:
        found.setdefault((literal['file'], literal['text']), []).append(literal['line'])
    # Every pinned row of one text in one file, so a text that occurs more
    # than once moves as a set: the rows keep their order.
    pinned = {}

    def pin(file, text, line, put):
        pinned.setdefault((file, text), []).append((line, put))

    for entry in entries['printed']:
        for source in entry['sources']:
            pin(source['file'], source['source'], source['line'], lambda line, s=source: s.__setitem__('line', line))
    for group in entries['unprinted']:
        for index, (line, text) in enumerate(group['literals']):
            pin(group['file'], text, line, lambda line, g=group, i=index, t=text: g['literals'].__setitem__(i, [line, t]))
    for entry in entries['unstaged']:
        pin(entry['file'], entry['text'], entry['line'], lambda line, e=entry: e.__setitem__('line', line))
    moved = 0
    for key, rows in pinned.items():
        lines = sorted(found.get(key, []))
        if len(lines) != len(rows) or sorted(line for line, _ in rows) == lines:
            continue
        for (old, put), line in zip(sorted(rows, key=lambda row: row[0]), lines):
            if old != line:
                put(line)
                moved += 1
    allowlist_path.write_text(dumped(entries))
    print(f'{moved} entries relocated', file=sys.stderr)


def dumped(allowlist):
    """The allowlist as the file keeps it: one entry per block, one literal per line."""
    def value(v):
        return json.dumps(v, ensure_ascii=False)
    blocks = []
    for name in ('printed', 'unprinted', 'unstaged'):
        items = []
        for entry in allowlist[name]:
            fields = []
            for key, field in entry.items():
                if key == 'literals':
                    rows = ',\n'.join(f'        {value(row)}' for row in field)
                    fields.append(f'      "literals": [\n{rows}\n      ]')
                elif key in ('sources', 'surfaces'):
                    rows = ',\n'.join(f'        {value(row)}' for row in field)
                    fields.append(f'      {value(key)}: [\n{rows}\n      ]')
                else:
                    fields.append(f'      {value(key)}: {value(field)}')
            items.append('    {\n' + ',\n'.join(fields) + '\n    }')
        blocks.append(f'  {value(name)}: [\n' + ',\n'.join(items) + '\n  ]')
    return '{\n' + ',\n'.join(blocks) + '\n}\n'


if __name__ == '__main__':
    root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 and not sys.argv[1].startswith('--') else ROOT
    literals = scan(root)
    if '--relocate' in sys.argv:
        relocate(Path(sys.argv[sys.argv.index('--relocate') + 1]), literals)
    else:
        json.dump(literals, sys.stdout, indent=1, ensure_ascii=False)
        print()
