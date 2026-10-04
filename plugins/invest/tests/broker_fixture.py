#!/usr/bin/env python3
"""External SDK transport fixture: prescribed broker records, no sizing or strategy logic.

It stands in for the SDK interpreter, so it receives the bridge's command line. A snapshot returns
the prescribed account, with quotes for the requested symbols and every held symbol (the bridge's
one quote call); a submit records the call and returns the next prescribed broker identity; a series
returns each requested symbol's prescribed probe and its daily bars inside the requested window, as
the SDK's by-date history would.
"""
import json
import os
import sqlite3
from pathlib import Path
import sys

home = Path(os.environ['HOME'])
args = sys.argv[1:]
method = next(x for x in args if x in ('snapshot', 'submit', 'series'))
request = json.loads(args[args.index('--request') + 1])
path = home / 'invest-broker.json'
state = json.loads(path.read_text())
log = home / 'invest-calls.jsonl'
observed = {}
if method == 'submit':
    db_path = Path.cwd() / 'ledger.sqlite3'
    with sqlite3.connect(db_path.resolve().as_uri() + '?mode=ro', uri=True) as db:
        row = db.execute("SELECT state, request FROM orders WHERE state='submitting'").fetchone()
        observed['persisted_state'] = row[0] if row else None
        observed['persisted_request'] = json.loads(row[1]) if row else None
with log.open('a') as f:
    f.write(json.dumps({'method': method, 'request': request,
                        'env_keys': sorted(os.environ), **observed}) + '\n')
if method == 'snapshot':
    snapshot = state['snapshot']
    wanted = set(request['symbols']) | set(snapshot['positions'])
    snapshot['quotes'] = {k: v for k, v in snapshot['quotes'].items() if k in wanted}
    print(json.dumps(snapshot))
elif method == 'series':
    prescribed = state.get('series', {})
    answers = {}
    for symbol in request['symbols']:
        answer = prescribed.get(symbol, {'error': 'no prescribed candlesticks'})
        if 'bars' in answer:
            answer = answer | {'bars': [b for b in answer['bars'] if request['start'] <= b[0] <= request['end']]}
        answers[symbol] = answer
    print(json.dumps({'series': answers}))
elif method == 'submit':
    if state.get('fail'):
        raise SystemExit(1)
    submits = sum(1 for line in log.read_text().splitlines() if json.loads(line)['method'] == 'submit')
    print(json.dumps(state.get('response', {'order_id': f'order-{submits}'})))
else:
    raise SystemExit(2)
