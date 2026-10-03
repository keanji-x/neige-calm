#!/usr/bin/env python3
"""Temporary #1983 experiment helper: apply one realistic edit to calm-server.

body <file> <fn> <tag>                     insert a black_box statement at the top of fn's body
sig  <file> <fn> <ty> <caller_file> <old> <new>   append `_incr_probe: <ty>` to fn's params and rewrite one call site
"""
import re, sys

def find_fn(src, name):
    m = re.search(r'\bfn ' + re.escape(name) + r'\s*\(', src)
    if not m:
        sys.exit(f'fn {name} not found')
    return m

def matching(src, i, open_c, close_c):
    depth = 0
    for j in range(i, len(src)):
        if src[j] == open_c:
            depth += 1
        elif src[j] == close_c:
            depth -= 1
            if depth == 0:
                return j
    sys.exit('unbalanced')

mode, path = sys.argv[1], sys.argv[2]
src = open(path).read()
m = find_fn(src, sys.argv[3])
close = matching(src, m.end() - 1, '(', ')')
if mode == 'body':
    brace = src.index('{', close)
    stmt = f'\n    ::std::hint::black_box("incr-probe-{sys.argv[4]}");'
    src = src[:brace + 1] + stmt + src[brace + 1:]
elif mode == 'sig':
    ty = sys.argv[4]
    params = src[m.end():close].rstrip()
    sep = '' if params.endswith(',') or params == '' else ','
    src = src[:m.end()] + params + f'{sep} _incr_probe: {ty}' + src[close:]
    open(path, 'w').write(src)
    caller, old, new = sys.argv[5], sys.argv[6], sys.argv[7]
    csrc = open(caller).read()
    if csrc.count(old) != 1:
        sys.exit(f'call site {old!r} count={csrc.count(old)}')
    open(caller, 'w').write(csrc.replace(old, new))
    sys.exit(0)
else:
    sys.exit('mode')
open(path, 'w').write(src)
