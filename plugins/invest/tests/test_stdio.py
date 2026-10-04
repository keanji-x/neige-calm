"""The production `run` entry point, driven over stdio as the kernel drives it."""
from datetime import datetime, timedelta, timezone
import json
import time

from host import Host
from recipe import unit_kinds
from rig import PLANNER, ROOT, SOURCES, WORKER


def wait_for(host, predicate):
    deadline = time.monotonic() + 20
    while True:
        state = host.tool('portfolio_status', {}, track='owner', caller=WORKER)['structuredContent']
        if predicate(state):
            return state
        assert time.monotonic() < deadline, state
        time.sleep(0.05)


def test_stdio_entrypoint_fences_roles_and_publishes_every_unit(rig):
    r = rig
    now = datetime.now(timezone.utc)
    r.quote('US:AAA', '100', at=now)
    r.opening(AAA=10)
    host = Host(r.home, r.root, r.values)
    try:
        listed = host.request('tools/list', {})['tools']
        manifest = json.loads((ROOT / 'manifest.json').read_text())['exposes_tools']
        assert [t['name'] for t in listed] == [t['name'] for t in manifest]
        wait_for(host, lambda s: s['snapshot'] is not None)
        args = {'decision_id': 'stdio-target', 'weights': [{'symbol': 'US:AAA', 'bps': 2000}],
                'message': 'Research and live market data support the target weights.',
                'source_refs': SOURCES, 'valid_until': (now + timedelta(hours=1)).isoformat()}
        refused = host.tool('decision_add', args, track='owner', caller=WORKER)
        assert refused['isError'] and refused['content'][0]['text'].startswith('decision_add: ')
        assert host.tool('portfolio_status', {}, track='owner')['isError']
        result = host.tool('decision_add', args, track='owner', caller=PLANNER)['structuredContent']
        assert result['decisions'][0]['state'] == 'queued'
        assert host.tool('execution_add', {'decision_id': 'stdio-target'}, track='owner', caller=PLANNER)['isError']
        result = host.tool('execution_add', {'decision_id': 'stdio-target'}, track='owner', caller=WORKER)
        assert result['structuredContent']['decisions'][0]['state'] == 'requested'
        # The request woke the loop that owns the broker write.
        wait_for(host, lambda s: s['decisions'][0]['state'] == 'working')
        assert [(o['symbol'], o['side'], o['quantity']) for o in r.submits()] == [('AAA.US', 'Buy', 12)]
        expected, deadline = unit_kinds(), time.monotonic() + 20
        while not expected <= {p['kind'] for p in host.overlays}:
            assert time.monotonic() < deadline, expected - {p['kind'] for p in host.overlays}
            host.receive()
        for overlay in host.overlays:
            assert set(overlay) == {'entity_kind', 'entity_id', 'kind', 'payload'}, overlay
            assert overlay['entity_kind'] == 'track' and overlay['entity_id'] == 'owner'
    finally:
        host.close()
