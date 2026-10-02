#!/usr/bin/env python3
"""External SDK transport fixture: prescribed broker records, no sizing/strategy logic."""
import json
import os
import sqlite3
from pathlib import Path
import sys

home = Path(os.environ['HOME'])
args = sys.argv[1:]
method = next(x for x in args if x in ('snapshot', 'submit'))
request = json.loads(args[args.index('--request') + 1])
path = home / 'allocation-broker.json'
state = json.loads(path.read_text())
observed = {}
if method == 'submit':
    db_path = Path.cwd() / 'spy-cash/ledger.sqlite3'
    with sqlite3.connect(db_path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        row = db.execute("SELECT id,state FROM decisions WHERE state='submitting'").fetchone()
        observed['persisted_state'] = row[1] if row else None
        saved = db.execute('SELECT body FROM order_requests WHERE id=?', (row[0],)).fetchone() if row else None
        observed['persisted_request'] = json.loads(saved[0]) if saved else None
with (home / 'allocation-calls.jsonl').open('a') as f:
    f.write(json.dumps({'method': method, 'request': request,
                        'env_keys': sorted(os.environ), **observed}) + '\n')
if method == 'snapshot':
    print(json.dumps(state['snapshot']))
elif method == 'submit':
    if state.get('publish'):
        state['snapshot']['orders'].append(state['publish'])
        path.write_text(json.dumps(state))
    if state.get('fail'):
        raise SystemExit(1)
    print(json.dumps(state.get('response', {'order_id': 'order-1'})))
else:
    raise SystemExit(2)
