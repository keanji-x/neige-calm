"""The production `run` entry point, driven over stdio as the kernel drives it."""
from datetime import datetime, timedelta, timezone
import json
import time

from host import Host
from recipe import INSTRUMENT, unit_kinds
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


def test_stdio_research_call_refuses_with_codes_and_projects_onto_the_caller(rig):
    r = rig
    now = datetime.now(timezone.utc)
    r.quote('US:AAA', '100', at=now)
    r.opening(AAA=10)  # covered from the start: live with key 1 after the first reconciliation
    host = Host(r.home, r.root, r.values)
    try:
        state = wait_for(host, lambda s: s['instruments'] and s['instruments'][0]['state'] == 'live')
        current = state['instruments'][0]['key']
        assert current == 'invest-US-AAA-1'

        def research(key, creator='owner'):
            return host.response('tools/call', {'name': 'instrument_status', 'arguments': {}, '_meta': {
                'dev.neige/track': {'id': 'research-aaa', 'creator_track_id': creator, 'creator_key': key},
                'dev.neige/caller': PLANNER}})

        for key, creator, code in ((current, 'elsewhere', -32403), ('invest-US-AAA-2', 'owner', -32403)):
            frame = research(key, creator)
            assert frame['error']['code'] == code and frame['error']['message'].startswith('instrument_status: ')
        assert not [o for o in host.overlays if o['entity_id'] == 'research-aaa']
        result = research(current)['result']['structuredContent']
        assert result['symbol'] == 'US:AAA' and result['position']['shares'] == 10
        # The view's units were set on the caller's own Track before the reply.
        projected = {o['kind'] for o in host.overlays if o['entity_id'] == 'research-aaa'}
        assert projected == unit_kinds(INSTRUMENT)
        version = wait_for(host, lambda s: s['instruments'][0]['last_seen_at'])['instruments'][0]['version']
        renewed = host.tool('instrument_set', {'symbol': 'US:AAA', 'expected_version': version,
                                               'message': 'Renew the research key.'}, track='owner', caller=PLANNER)
        assert renewed['structuredContent']['track_add']['idempotency_key'] == 'invest-US-AAA-2'
        # A dropped symbol supersedes its research Track.
        r.quote('US:BBB', '50', at=now)
        host.tool('instrument_add', {'symbol': 'US:BBB', 'message': 'Watch BBB.'}, track='owner', caller=PLANNER)
        bbb = wait_for(host, lambda s: any(i['symbol'] == 'US:BBB' and i['state'] == 'live' for i in s['instruments']))
        bbb = next(i for i in bbb['instruments'] if i['symbol'] == 'US:BBB')
        host.tool('instrument_rm', {'symbol': 'US:BBB', 'expected_version': bbb['version'], 'message': 'Stop BBB.'},
                  track='owner', caller=PLANNER)
        frame = research(bbb['key'])
        assert frame['error']['code'] == -32409 and 'superseded' in frame['error']['message']
    finally:
        host.close()
