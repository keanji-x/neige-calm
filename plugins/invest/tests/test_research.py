"""Research Tracks: provenance attestation, one issued key per live symbol, and the lease (#2104 §3.3)."""
from datetime import timedelta

from invest.errors import CONFLICT, FORBIDDEN
from invest.ledger import encoded
from rig import NOW, OWNER, PLANNER, SOURCES, refusal, track


def refused(code, call, *args, **kwargs):
    with refusal(code) as caught:
        call(*args, **kwargs)
    return str(caught.value)


def refusal_data(call, *args, **kwargs):
    with refusal(CONFLICT) as caught:
        call(*args, **kwargs)
    return caught.value.data


def key(r, symbol):
    return r.instrument(symbol)['key']


def test_attestation_requires_portfolio_creator_and_current_key(rig):
    r = rig
    r.cover('US:AAA', 'US:BBB')
    r.thesis('aaa-margins', 'US:AAA')
    r.add('US:CCC')  # added, not yet verified: pending, no key issued
    assert r.instrument('US:CCC')['state'] == 'pending' and r.instrument('US:CCC')['key'] is None
    before = r.dump()
    thesis = r.open_thesis('aaa-margins')
    args = {'thesis_id': 'aaa-margins', 'assessment': 'holding', 'summary': 'Margins held up this quarter.',
            'source_refs': SOURCES, 'expected_version': thesis['version']}
    # A foreign creator holding S's current key.
    message = refused(FORBIDDEN, r.research, key(r, 'US:AAA'), 'thesis_set', args, creator='elsewhere')
    assert message.startswith('plugin_invest_thesis_set: ')
    refused(FORBIDDEN, r.research, key(r, 'US:AAA'), 'instrument_status', creator='elsewhere')
    # S's thesis written with the current key of another symbol.
    refused(FORBIDDEN, r.research, key(r, 'US:BBB'), 'thesis_set', args)
    # A pending S: no key is issued yet, so a Track under its first key holds nothing.
    for name, call_args in (('instrument_status', {}), ('thesis_set', args)):
        refused(FORBIDDEN, r.research, 'invest-US-CCC-1', name, call_args)
    # The portfolio Track itself has no creator.
    refused(FORBIDDEN, r.app.call, OWNER, 'thesis_set', args, PLANNER)
    refused(FORBIDDEN, r.app.call, OWNER, 'instrument_status', {}, PLANNER)
    assert r.dump() == before
    assert r.instrument('US:AAA')['last_seen_at'] is None, 'a refused call is never seen'
    # Exactly the portfolio-created Track holding S's current key may assess S.
    view = r.research(key(r, 'US:AAA'), 'thesis_set', args)
    assert view['symbol'] == 'US:AAA'
    assert [(t['thesis_id'], t['assessment'], t['version']) for t in view['theses']] == [('aaa-margins', 'holding', 2)]


def test_superseded_key_refused(rig):
    r = rig
    r.cover('US:AAA')
    r.thesis('aaa-margins', 'US:AAA')
    old = key(r, 'US:AAA')
    assert old == 'invest-US-AAA-1'
    r.research(old, 'instrument_status')
    renewed = r.renew('US:AAA')['track_add']['idempotency_key']
    assert renewed == 'invest-US-AAA-2'
    thesis = r.open_thesis('aaa-margins')
    args = {'thesis_id': 'aaa-margins', 'assessment': 'broken', 'summary': 'An older Track must not write this.',
            'source_refs': SOURCES, 'expected_version': thesis['version']}
    before = r.dump()
    for name, call_args in (('instrument_status', {}), ('thesis_set', args)):
        message = refused(CONFLICT, r.research, old, name, call_args)
        assert 'superseded: close this Track' in message and renewed in message
    assert refusal_data(r.research, old, 'instrument_status') == {
        'refusal': 'superseded', 'symbol': 'US:AAA', 'key': renewed}
    assert r.dump() == before
    # The current key is attested; then a dropped S supersedes every key, the current one included.
    assert r.research(renewed, 'instrument_status')['symbol'] == 'US:AAA'
    r.remove('US:AAA')
    for stale_key in (old, renewed):
        assert 'superseded' in refused(CONFLICT, r.research, stale_key, 'instrument_status')
    # Re-added, S is pending with no current key: its last key stays superseded until n+1 is issued.
    r.add('US:AAA')
    assert r.instrument('US:AAA')['state'] == 'pending'
    assert 'superseded' in refused(CONFLICT, r.research, renewed, 'instrument_status')


def test_never_seen_key_goes_stale(rig):
    r = rig
    r.cover('US:AAA')  # key 1 issued at NOW
    for minutes, stale in ((0, False), (119, False), (121, True)):
        r.clock = lambda minutes=minutes: NOW + timedelta(minutes=minutes)
        assert r.instrument('US:AAA')['stale'] is stale, minutes
    # Its first attested call is the binding proof; the lease then runs from it.
    r.research(key(r, 'US:AAA'), 'instrument_status')
    assert r.instrument('US:AAA')['stale'] is False


def test_lease_expiry_goes_stale(rig):
    r = rig
    r.configure(lease_days=3)
    r.cover('US:AAA')
    current = key(r, 'US:AAA')
    r.research(current, 'instrument_status')
    for days, stale in ((2, False), (4, True)):
        r.clock = lambda days=days: NOW + timedelta(days=days)
        assert r.instrument('US:AAA')['stale'] is stale, days
    # Staleness is a lease, not a revocation: the current Track's next call renews it.
    r.research(current, 'instrument_status')
    assert r.instrument('US:AAA')['stale'] is False


def test_set_issues_next_key_with_byte_identical_args(rig):
    r = rig
    r.cover('US:AAA')
    first = r.instrument('US:AAA')['track_add']
    assert set(first) == {'recipe_id', 'title', 'idempotency_key', 'text', 'message'}
    assert (first['recipe_id'], first['title'], first['idempotency_key']) == (
        'recipe-instrument', 'US:AAA 研究', 'invest-US-AAA-1')
    assert 'invest-US-AAA-1' in first['text'] and 'instrument_status' in first['text']
    # A retry within n passes the same bytes: the ledger keeps the exact arguments it issued, so a
    # restart under another configured recipe still answers them unchanged.
    r.configure(instrument_recipe_id='recipe-instrument-v2')
    assert encoded(r.instrument('US:AAA')['track_add']) == encoded(first)
    row = r.instrument('US:AAA')
    with refusal(CONFLICT, 'expected_version'):
        r.app.call(OWNER, 'instrument_set', {'symbol': 'US:AAA', 'expected_version': row['version'] - 1,
                                             'message': 'Renew under a stale version.'}, PLANNER)
    assert r.instrument('US:AAA') == row
    result = r.app.call(OWNER, 'instrument_set', {'symbol': 'us:aaa', 'expected_version': row['version'],
                                                  'message': 'Renew: the research Track went silent.'}, PLANNER)
    renewed = result['track_add']
    assert renewed['idempotency_key'] == 'invest-US-AAA-2' and renewed['recipe_id'] == 'recipe-instrument-v2'
    assert renewed['title'] == first['title'] and 'invest-US-AAA-2' in renewed['text']
    after = r.instrument('US:AAA')
    assert encoded(after['track_add']) == encoded(renewed)
    assert (after['key'], after['key_seq'], after['version'], after['last_seen_at']) == (
        'invest-US-AAA-2', 2, row['version'] + 1, None)
    # Only a live symbol has a key to replace.
    r.add('US:BBB')
    with refusal(CONFLICT, 'live'):
        r.renew('US:BBB')


def test_views_change_no_domain_state(rig):
    r = rig
    r.cover('US:AAA')
    r.thesis('aaa-margins', 'US:AAA')
    current = key(r, 'US:AAA')
    before = r.dump()
    r.status()
    view = r.research(current, 'instrument_status')
    r.research(current, 'instrument_status')
    r.status()
    assert r.dump() == before
    assert view['symbol'] == 'US:AAA' and [t['thesis_id'] for t in view['theses']] == ['aaa-margins']
    # The one write a view makes is the access stamp on the caller's own instrument.
    assert r.instrument('US:AAA')['last_seen_at'] == NOW.isoformat()


def test_last_seen_never_changes_authority(rig):
    r = rig
    r.cover('US:AAA')
    current = key(r, 'US:AAA')
    version = r.instrument('US:AAA')['version']
    # A never-seen current key is attested at once; being seen bumps no version.
    r.research(current, 'instrument_status')
    seen = r.instrument('US:AAA')
    assert seen['last_seen_at'] is not None and seen['version'] == version
    # A fresh stamp gives no other caller authority over S.
    refused(FORBIDDEN, r.research, current, 'instrument_status', creator='elsewhere')
    refused(FORBIDDEN, r.app.call, OWNER, 'instrument_status', {}, PLANNER)
    # A long-unseen current key keeps its authority, and its next call renews the lease.
    r.clock = lambda: NOW + timedelta(days=30)
    assert r.instrument('US:AAA')['stale'] is True
    r.research(current, 'instrument_status')
    after = r.instrument('US:AAA')
    assert after['stale'] is False and after['version'] == version and after['key'] == current


def test_thesis_set_requires_current_version(rig):
    r = rig
    r.cover('US:AAA')
    r.thesis('aaa-margins', 'US:AAA')
    current = key(r, 'US:AAA')
    r.assess(current, 'aaa-margins', 'holding')
    before = r.dump()
    # A lost answer retried under the version it read first, or any other stale version.
    for stale in (1, 3):
        assert 'the current version is 2' in refused(CONFLICT, r.assess, current, 'aaa-margins', 'broken',
                                                     expected_version=stale)
    assert refusal_data(r.assess, current, 'aaa-margins', 'broken', expected_version=1) == {
        'refusal': 'stale_version', 'version': 2}
    assert r.dump() == before
    assert r.open_thesis('aaa-margins')['assessment'] == 'holding'


def test_track_context_requires_provenance_and_ignores_extra_keys(rig):
    r = rig
    r.cover('US:AAA')
    current = key(r, 'US:AAA')
    extra = track('research-aaa', 'owner', current) | {'created_at': 1}
    view = r.app.call(extra, 'instrument_status', {}, PLANNER)
    assert view['symbol'] == 'US:AAA' and view['portfolio_track_id'] == 'owner'  # the research report's link
    assert r.app.call(OWNER | {'created_at': 1}, 'portfolio_status', {}, PLANNER)['instruments']
    missing = {'id': 'research-aaa', 'creator_track_id': 'owner'}
    refused(FORBIDDEN, r.app.call, missing, 'instrument_status', {}, PLANNER)


def test_research_status_prices_a_watched_symbol_it_does_not_hold(rig):
    # The research recipe reads the symbol's price from here, never from a broker CLI.
    r = rig
    r.cover('US:AAA', price='123.45')
    view = r.research(key(r, 'US:AAA'), 'instrument_status')
    assert not view['held'] and view['position']['shares'] == 0 and view['position']['price'] == '123.45'
