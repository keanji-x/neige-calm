import { describe, expect, it } from 'vitest';

import {
  activeTracksOn, createCardOperation, createCodexCardOperation, createTerminalCardOperation,
  createTrackOperation, deleteCardOperation, hasFailed, isBlankForKernel, isClosed,
  isWorking, needsUserAttention, toTrack, trackActivityFrom,
  sortAreaTracksByRecent, trackRecentAt, limitAreaTracks, railAreaTracks, AREA_TRACK_LIMIT,
  trackActivityState, trackDetailSchema, updateTrackOperation,
  NEUTRAL_ACTIVITY, UNTITLED_TRACK_LABEL, trackDisplayTitle, trackWireSchema, tracksInAreaOperation,
  userVisibleTracks, trackOverlayPayload, plannerProviderOf,
  type Track, type OverlayWire,
} from './track.js';
import type { Area } from './area.js';

const baseWire = {
  id: 'w1', area_id: 'c1', title: 'Ship it', sort: 1,
  created_at: 1_000, updated_at: 1_000,
};

function track(overrides: Partial<Track>): Track {
  return {
    id: 'w', areaId: 'c', title: 't', sort: 1, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

const DAY = 24 * 60 * 60 * 1000;

describe('track wire decode', () => {
  it('fills the kernel serde defaults so the decoded track has no optional fields', () => {
    const parsed = trackWireSchema.parse(baseWire);
    expect(parsed).toMatchObject({
      cwd: '', pinned_at: null, closed_at: null,
    });
  });

  it('keeps explicit wire values over the defaults', () => {
    const parsed = trackWireSchema.parse({ ...baseWire, cwd: '/srv', closed_at: 7 });
    expect(parsed.cwd).toBe('/srv');
    expect(parsed.closed_at).toBe(7);
  });

  it('drops server fields this slice does not model instead of failing the decode', () => {
    expect(trackWireSchema.safeParse({ ...baseWire, template_id: null, purpose: null }).success).toBe(true);
  });

  it('maps the wire row onto the camelCase domain shape', () => {
    expect(toTrack(trackWireSchema.parse({ ...baseWire, pinned_at: 42 }))).toEqual(track({
      id: 'w1', areaId: 'c1', title: 'Ship it', cwd: '', agentCwd: '', pinnedAt: 42, createdAt: 1_000,
      updatedAt: 1_000,
    }));
  });

  it('roots the agent at the track worktree when the kernel made one (#1830), else at the checkout', () => {
    const worktree = '/repo/.claude/worktrees/track-w1';
    const attached = toTrack(trackWireSchema.parse({
      ...baseWire, cwd: '/repo', workspace: { kind: 'attached', path: '/repo', frozen_at: 1, worktree },
    }));
    expect(attached.cwd).toBe('/repo');
    expect(attached.agentCwd).toBe(worktree);
    const managed = toTrack(trackWireSchema.parse({
      ...baseWire, cwd: '/w', workspace: { kind: 'managed', path: '/w', frozen_at: null },
    }));
    expect(managed.agentCwd).toBe('/w');
  });

  it('percent-encodes the area id into the list path', () => {
    expect(tracksInAreaOperation('a/b').path).toBe('/api/areas/a%2Fb/tracks');
  });
});

describe('plannerProviderOf', () => {
  it('reads the server-owned key, and Codex for anything else', () => {
    expect(plannerProviderOf({ planner_harness: true, planner_provider: 'claude' })).toBe('claude');
    expect(plannerProviderOf({ planner_harness: true, planner_provider: 'codex' })).toBe('codex');
    expect(plannerProviderOf({ planner_harness: true })).toBe('codex');
    expect(plannerProviderOf(null)).toBe('codex');
  });
});

describe('track create operation', () => {
  const theme = { fg: [1, 2, 3], bg: [4, 5, 6] } as const;

  it('carries the draft key exactly when a first message is present', () => {
    const keyed = createTrackOperation(
      { area_id: 'area', planner_provider: 'codex', theme, first_message: 'ship it' },
      'draft-key',
    );
    expect(keyed.headers).toEqual({ 'Idempotency-Key': 'draft-key' });

    const messageLess = createTrackOperation({ area_id: 'area', planner_provider: 'codex', theme });
    expect(messageLess.headers).toBeUndefined();
  });

  it('makes a message without a key invalid at both the type and runtime boundaries', () => {
    expect(() => {
      // @ts-expect-error first_message makes Idempotency-Key required.
      createTrackOperation({ area_id: 'area', planner_provider: 'codex', theme, first_message: 'missing key' });
    }).toThrow(/Idempotency-Key/);
  });
});

describe('card operations', () => {
  const theme = { fg: [1, 2, 3], bg: [4, 5, 6] } as const;

  it('deletes a card by id on DELETE /api/cards/:id', () => {
    const operation = deleteCardOperation('card 1/2');
    expect(operation.method).toBe('DELETE');
    expect(operation.path).toBe('/api/cards/card%201%2F2');
    expect('body' in operation).toBe(false);
  });

  it('mints a codex card on the kind\'s own atomic endpoint, carrying the body verbatim', () => {
    const body = { theme, title: 'Codex', cwd: '/srv' };
    const operation = createCodexCardOperation('w/1', body);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/w%2F1/codex-cards');
    expect(operation.body).toBe(body);
  });

  it('mints a terminal card on its own atomic endpoint, not the generic one', () => {
    const operation = createTerminalCardOperation('w1', { theme });
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/w1/terminal-cards');
  });

  it('writes a runtime-less card through the generic create with its kind and payload', () => {
    const body = { kind: 'file-viewer', payload: { path: '/repo/notes.md' }, title: 'Notes' };
    const operation = createCardOperation('w/1', body);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/w%2F1/cards');
    expect(operation.body).toBe(body);
  });
});

describe('open and closed', () => {
  it('reads closed from closedAt alone', () => {
    expect(isClosed(track({ closedAt: null }))).toBe(false);
    expect(isClosed(track({ closedAt: 5 }))).toBe(true);
  });

  it('requires the server-derived Reopen and Close capabilities on track detail', () => {
    const detail = { track: { ...baseWire }, cards: [], overlays: [] };
    expect(trackDetailSchema.safeParse(detail).success).toBe(false);
    expect(trackDetailSchema.safeParse({ ...detail, can_reopen: true }).success).toBe(false);
    expect(trackDetailSchema.safeParse({ ...detail, can_close: true }).success).toBe(false);
    expect(trackDetailSchema.parse({ ...detail, can_reopen: true, can_close: false }))
      .toMatchObject({ can_reopen: true, can_close: false });
  });

  it('builds the reopen PATCH without a parallel endpoint', () => {
    const operation = updateTrackOperation('w/1', { closed: false });
    expect(operation).toMatchObject({
      method: 'PATCH', path: '/api/tracks/w%2F1', body: { closed: false },
    });
  });

  it('falls back to a single untitled label', () => {
    expect(trackDisplayTitle('   ')).toBe(UNTITLED_TRACK_LABEL);
    expect(trackDisplayTitle(' Ship ')).toBe('Ship');
  });
});

describe('activity predicates read only the kernel activity overlay (INV-APP-118)', () => {
  it('does not derive attention or failure from the open/closed state', () => {
    const closed = track({ closedAt: 5, attention: 'none' });
    expect(needsUserAttention(closed)).toBe(false);
    expect(hasFailed(closed)).toBe(false);
    expect(trackActivityState(closed, false)).toBe('quiet');
  });

  it('does not derive working from an open track', () => {
    expect(isWorking(track({ closedAt: null, working: false }))).toBe(false);
    expect(trackActivityState(track({ closedAt: null, working: false }), false)).toBe('quiet');
    expect(isWorking(track({ closedAt: 5, working: true }))).toBe(true);
    expect(trackActivityState(track({ closedAt: 5, working: true }), false)).toBe('working');
  });

  it('reads attention and failure from the overlay verdict', () => {
    expect(needsUserAttention(track({ attention: 'input' }))).toBe(true);
    expect(hasFailed(track({ attention: 'input' }))).toBe(false);
    expect(hasFailed(track({ closedAt: 5, attention: 'failed' }))).toBe(true);
    expect(needsUserAttention(track({ closedAt: 5, attention: 'failed' }))).toBe(false);
    expect(trackActivityState(track({ attention: 'input', working: true }), true)).toBe('attention');
    expect(trackActivityState(track({ attention: 'failed', working: true }), true)).toBe('failed');
    expect(trackActivityState(track({ attention: 'none', working: false }), true)).toBe('unread');
  });
});

describe('trackActivityFrom: the kernel activity overlay', () => {
  const overlay = (payload: unknown, over: Partial<OverlayWire> = {}): OverlayWire => ({
    id: 'a1', plugin_id: 'kernel', entity_kind: 'track', entity_id: 't1', kind: 'activity',
    payload, updated_at: 1, ...over,
  });
  const payload = {
    schemaVersion: 2, working: true, attention: 'failed', activity_at_ms: 1_789_460_968_837,
    items: [
      { source: 'planner_down', key: 'planner_down:22825', text: 'unexpected status 403 Forbidden', at_ms: 20 },
      { source: 'ask', key: 'ask:notify:22801', text: 'Which branch?', at_ms: 10 },
      { source: 'ask', key: 'ask:ratify:28475', text: 'Merge the PR?', at_ms: 5 },
    ],
    cards: [{ card_id: 'worker-1', state: 'failed' }, { card_id: 'planner', state: 'input' }, { card_id: 'w2', state: 'working' }],
  };

  it('decodes working, attention, the high-water mark, the items and the per-card verdicts', () => {
    const activity = trackActivityFrom('t1', [overlay(payload)]);
    expect(activity.working).toBe(true);
    expect(activity.attention).toBe('failed');
    expect(activity.activityAt).toBe(1_789_460_968_837);
    expect(activity.recentAt).toBe(1_789_460_968_837);
    expect(activity.attentionItems).toEqual([
      { source: 'planner_down', key: 'planner_down:22825', text: 'unexpected status 403 Forbidden', atMs: 20 },
      { source: 'ask', key: 'ask:notify:22801', text: 'Which branch?', atMs: 10 },
      { source: 'ask', key: 'ask:ratify:28475', text: 'Merge the PR?', atMs: 5 },
    ]);
    expect(activity.cards).toEqual({ 'worker-1': 'failed', planner: 'input', w2: 'working' });
    expect(activity.progress).toBe(0);
  });

  it('keeps the neutral values for a track the kernel has not written yet', () => {
    const activity = trackActivityFrom('t2', [overlay(payload)]);
    expect(activity).toEqual(NEUTRAL_ACTIVITY);
    expect(activity.activityAt).toBeNull();
  });

  it('drops a malformed row and keeps the rest of the payload', () => {
    const activity = trackActivityFrom('t1', [overlay({
      ...payload,
      items: [{ source: 'ask', key: 'ask:ratify:1' }, ...payload.items],
      cards: [{ card_id: 'w9', state: 'sleeping' }, { card_id: 'w2', state: 'working' }],
    })]);
    expect(activity.attentionItems).toHaveLength(3);
    expect(activity.cards).toEqual({ w2: 'working' });
    expect(activity.working).toBe(true);
  });

  it('ignores a payload that is not the v2 shape at all', () => {
    for (const junk of [null, 'working', { schemaVersion: 2, working: true }, { working: 'yes' }]) {
      expect(trackActivityFrom('t1', [overlay(junk)])).toEqual({ ...NEUTRAL_ACTIVITY, recentAt: 1 });
    }
  });

  /* An older kernel's row: its items meant something else, so the whole row reads as no overlay. */
  it('v1 activity overlay is ignored', () => {
    const v1 = {
      schemaVersion: 1, working: true, attention: 'failed', activity_at_ms: 1_789_460_968_837,
      items: [{ kind: 'failed', source: 'session', id: 'ws-1', card_id: 'planner', at_ms: 20 }],
      cards: [{ card_id: 'planner', state: 'failed' }],
    };
    expect(trackActivityFrom('t1', [overlay(v1)])).toEqual({ ...NEUTRAL_ACTIVITY, recentAt: 1 });
  });

  it('uses every matching row time and valid activity time without trusting input order', () => {
    const olderPayload = overlay({ ...payload, activity_at_ms: 40 }, { id: 'older', updated_at: 50 });
    const newerRow = overlay({ schemaVersion: 2 }, { id: 'newer', updated_at: 90 });
    const nonFinite = overlay({ ...payload, activity_at_ms: Number.POSITIVE_INFINITY }, {
      id: 'non-finite', updated_at: Number.NaN,
    });
    for (const rows of [[olderPayload, newerRow, nonFinite], [nonFinite, newerRow, olderPayload]]) {
      expect(trackActivityFrom('t1', rows).recentAt).toBe(90);
    }
  });

  it('is a plain per-track object, not a shared container', () => {
    const first = trackActivityFrom('t1', [overlay(payload)]);
    const second = trackActivityFrom('t1', [overlay({ ...payload, cards: [] })]);
    expect(first.cards).not.toBe(second.cards);
    expect(second.cards).toEqual({});
    expect(NEUTRAL_ACTIVITY.cards).toEqual({});
  });

  it('applies only the kernel-owned activity row; a plugin-owned one leaves the track quiet', () => {
    // A plugin can write `kind: 'activity'` under its own id; that row is not the verdict.
    const impostor = overlay(payload, { id: 'a2', plugin_id: 'dev.echo' });
    expect(trackActivityFrom('t1', [impostor])).toEqual(NEUTRAL_ACTIVITY);
    expect(trackActivityFrom('t1', [impostor]).working).toBe(false);
    const kernel = overlay({ ...payload, working: false, attention: 'none', items: [], cards: [] });
    for (const rows of [[impostor, kernel], [kernel, impostor]]) {
      const activity = trackActivityFrom('t1', rows);
      expect(activity.working).toBe(false);
      expect(activity.attention).toBe('none');
      expect(activity.cards).toEqual({});
    }
    expect(trackActivityFrom('t1', [overlay({ value: 0.5 }, { kind: 'progress', plugin_id: 'dev.echo' })]).progress).toBe(0.5);
  });
});

describe('Area recent-activity order', () => {
  it('orders by the newest finite row or activity time, then sort and bytewise id', () => {
    const rows = [
      track({ id: 'b', sort: 1, updatedAt: 20, recentAt: 100 }),
      track({ id: 'a', sort: 1, updatedAt: 100, recentAt: 30 }),
      track({ id: 'z', sort: 0, updatedAt: 100, recentAt: null }),
      track({ id: 'bad', sort: 0, updatedAt: Number.NaN, createdAt: 5, recentAt: Number.POSITIVE_INFINITY }),
    ];
    expect(sortAreaTracksByRecent(rows).map((row) => row.id)).toEqual(['z', 'a', 'b', 'bad']);
    expect(trackRecentAt(rows[3])).toBe(5);
  });

  it('returns a new array without mutating its source or Track objects', () => {
    const old = track({ id: 'old', sort: 1, updatedAt: 1 });
    const recent = track({ id: 'recent', sort: 2, updatedAt: 2 });
    const source = [old, recent];
    const sorted = sortAreaTracksByRecent(source);
    expect(sorted.map((row) => row.id)).toEqual(['recent', 'old']);
    expect(source).toEqual([old, recent]);
    expect(sorted[0]).toBe(recent);
  });
});

describe('limitAreaTracks', () => {
  const ids = (count: number) => Array.from({ length: count }, (_, index) => track({ id: `t${index + 1}` }));
  const shown = (rows: readonly Track[]) => rows.map((row) => row.id);

  it('keeps the first five in order and counts the rest as hidden', () => {
    expect(AREA_TRACK_LIMIT).toBe(5);
    const limited = limitAreaTracks(ids(7), AREA_TRACK_LIMIT, null);
    expect(shown(limited.rows)).toEqual(['t1', 't2', 't3', 't4', 't5']);
    expect(limited.hiddenCount).toBe(2);
  });

  it('hides nothing at or under the limit', () => {
    for (const count of [0, 1, 5]) {
      const limited = limitAreaTracks(ids(count), AREA_TRACK_LIMIT, null);
      expect(shown(limited.rows), `${count}`).toEqual(shown(ids(count)));
      expect(limited.hiddenCount, `${count}`).toBe(0);
    }
  });

  it('does not repeat an open Track that is already among the first five', () => {
    const limited = limitAreaTracks(ids(7), AREA_TRACK_LIMIT, 't3');
    expect(shown(limited.rows)).toEqual(['t1', 't2', 't3', 't4', 't5']);
    expect(limited.hiddenCount).toBe(2);
  });

  it('keeps an open sixth Track, leaving nothing hidden', () => {
    const limited = limitAreaTracks(ids(6), AREA_TRACK_LIMIT, 't6');
    expect(shown(limited.rows)).toEqual(['t1', 't2', 't3', 't4', 't5', 't6']);
    expect(limited.hiddenCount).toBe(0);
  });

  it('keeps an open seventh Track at its own position, last, without counting it as hidden', () => {
    const limited = limitAreaTracks(ids(8), AREA_TRACK_LIMIT, 't7');
    expect(shown(limited.rows)).toEqual(['t1', 't2', 't3', 't4', 't5', 't7']);
    expect(limited.hiddenCount).toBe(2);
    const seven = limitAreaTracks(ids(7), AREA_TRACK_LIMIT, 't7');
    expect(shown(seven.rows)).toEqual(['t1', 't2', 't3', 't4', 't5', 't7']);
    expect(seven.hiddenCount).toBe(1);
  });

  it('ignores an open Track that is not in this Area', () => {
    const limited = limitAreaTracks(ids(7), AREA_TRACK_LIMIT, 'elsewhere');
    expect(shown(limited.rows)).toEqual(['t1', 't2', 't3', 't4', 't5']);
    expect(limited.hiddenCount).toBe(2);
  });

  it('leaves its input untouched', () => {
    const source = ids(7);
    const before = [...source];
    const limited = limitAreaTracks(source, AREA_TRACK_LIMIT, 't7');
    expect(source).toEqual(before);
    expect(limited.rows).not.toBe(source);
    expect(limited.rows[0]).toBe(source[0]);
  });
});

describe('railAreaTracks', () => {
  // t1 open; t2 closed and unread; t3 closed and open in the view; t4 closed, read and elsewhere.
  const source = [
    track({ id: 't1' }),
    track({ id: 't2', closedAt: 5 }),
    track({ id: 't3', closedAt: 5 }),
    track({ id: 't4', closedAt: 5 }),
  ];
  const unread = (row: Track) => row.id === 't2';
  const shown = (rows: readonly Track[]) => rows.map((row) => row.id);

  it('railAreaTracks keeps open, unread and active tracks', () => {
    expect(shown(railAreaTracks(source, 't3', unread, false))).toEqual(['t1', 't2', 't3']);
    expect(shown(railAreaTracks(source, null, unread, false))).toEqual(['t1', 't2']);
    expect(shown(railAreaTracks(source, null, () => false, false))).toEqual(['t1']);
  });

  it('keeps every track, in order, when the Area shows closed ones', () => {
    expect(shown(railAreaTracks(source, null, () => false, true))).toEqual(['t1', 't2', 't3', 't4']);
  });

  it('leaves its input untouched', () => {
    const before = [...source];
    const rows = railAreaTracks(source, null, () => false, true);
    expect(source).toEqual(before);
    expect(rows).not.toBe(source);
  });
});

describe('activeTracksOn', () => {
  const day = new Date(2026, 7, 10, 12, 0, 0);
  const dayStart = new Date(2026, 7, 10, 0, 0, 0).getTime();
  const dayEnd = new Date(2026, 7, 10, 23, 59, 59, 999).getTime();
  const now = day.getTime();

  it('includes an open track created before the day and still running', () => {
    const open = track({ id: 'open', createdAt: dayStart - DAY, closedAt: null });
    expect(activeTracksOn([open], day, now).map((w) => w.id)).toEqual(['open']);
  });

  it('includes a track created in the last millisecond of the day', () => {
    const late = track({ id: 'late', createdAt: dayEnd, closedAt: null });
    expect(activeTracksOn([late], day, now).map((w) => w.id)).toEqual(['late']);
  });

  it('includes a track that ended exactly at the start of the day', () => {
    const edge = track({ id: 'edge', createdAt: dayStart - DAY, closedAt: dayStart });
    expect(activeTracksOn([edge], day, now).map((w) => w.id)).toEqual(['edge']);
  });

  it('excludes a track that ended before the day and one created after it', () => {
    const before = track({ id: 'before', createdAt: dayStart - 2 * DAY, closedAt: dayStart - 1 });
    const after = track({ id: 'after', createdAt: dayEnd + 1, closedAt: null });
    expect(activeTracksOn([before, after], day, now)).toEqual([]);
  });

  it('orders oldest first and breaks ties by id', () => {
    const b = track({ id: 'b', createdAt: dayStart + 10 });
    const a = track({ id: 'a', createdAt: dayStart + 10 });
    const older = track({ id: 'z', createdAt: dayStart + 1 });
    expect(activeTracksOn([b, a, older], day, now).map((w) => w.id)).toEqual(['z', 'a', 'b']);
  });

  it('does not mutate the input list', () => {
    const list = [track({ id: 'b', createdAt: dayStart + 2 }), track({ id: 'a', createdAt: dayStart + 1 })];
    activeTracksOn(list, day, now);
    expect(list.map((w) => w.id)).toEqual(['b', 'a']);
  });
});

describe('userVisibleTracks', () => {
  const userArea: Area = {
    id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
  };
  const systemArea: Area = {
    id: 'sys', name: 'Kernel', color: '#000000', sort: 0, kind: 'system',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
  };
  const mine = track({ id: 'w1', areaId: 'c1' });
  const scaffolding = track({ id: 'w-sys', areaId: 'sys' });
  const closed = track({ id: 'w2', areaId: 'c1', closedAt: 1 });

  it('[E2E-INV-SHELL-003] drops tracks hosted by the system area', () => {
    expect(userVisibleTracks([mine, scaffolding], [userArea, systemArea]).map((w) => w.id))
      .toEqual(['w1']);
  });

  it('keeps closed tracks and drops tracks whose area is absent from the list', () => {
    expect(userVisibleTracks([mine, closed], [userArea]).map((w) => w.id)).toEqual(['w1', 'w2']);
    expect(userVisibleTracks([mine], [])).toEqual([]);
  });
});

/* The kernel trims on the Unicode `White_Space` property, which JS `trim()` does not match exactly. */
describe('isBlankForKernel', () => {
  it('is true for the empty string and for ordinary JS whitespace', () => {
    expect(isBlankForKernel('')).toBe(true);
    expect(isBlankForKernel('   ')).toBe(true);
    expect(isBlankForKernel('\t\n\r ')).toBe(true);
  });

  it('is true for U+00A0 NO-BREAK SPACE, which both sides call whitespace', () => {
    expect(isBlankForKernel('\u00A0')).toBe(true);
  });

  it('is true for U+0085 NEXT LINE, which JS trim() leaves standing', () => {
    expect(isBlankForKernel('\u0085')).toBe(true);
    expect('\u0085'.trim()).not.toBe('');
  });

  it('is false as soon as there is anything to say', () => {
    expect(isBlankForKernel('hi')).toBe(false);
    expect(isBlankForKernel('  keep indentation  ')).toBe(false);
    expect(isBlankForKernel('\u0085x\u0085')).toBe(false);
  });
});

describe('trackOverlayPayload', () => {
  const overlay = (over: Partial<OverlayWire>): OverlayWire => ({
    id: 'o1',
    plugin_id: 'dev-neige-binance',
    entity_kind: 'track',
    entity_id: 't1',
    kind: 'portfolio.holdings',
    payload: { columns: [], rows: [] },
    updated_at: 1,
    ...over,
  });
  const SOURCE = 'neige://plugin/dev-neige-binance/portfolio.holdings';

  it('returns the payload of the overlay the source addresses', () => {
    expect(trackOverlayPayload('t1', [overlay({})], SOURCE))
      .toEqual({ columns: [], rows: [] });
  });

  it('does not cross tracks, plugins, kinds or entity kinds', () => {
    // Each of these differs from the addressed overlay in exactly one field.
    const wrong = [
      overlay({ entity_id: 't2' }),
      overlay({ plugin_id: 'other-plugin' }),
      overlay({ kind: 'portfolio.history' }),
      overlay({ entity_kind: 'card' }),
    ];
    for (const row of wrong) {
      expect(trackOverlayPayload('t1', [row], SOURCE)).toBeUndefined();
    }
    expect(trackOverlayPayload('t1', [...wrong, overlay({})], SOURCE))
      .toEqual({ columns: [], rows: [] });
  });

  it('returns undefined for a source that is not a plugin overlay reference', () => {
    for (const source of [
      'neige://track/t1#b_x',
      'https://example.com/x',
      'neige://plugin/only-one',
      'neige://plugin/a/b/c',
      'neige://plugin//portfolio.holdings',
      'neige://plugin/dev-neige-binance/',
    ]) {
      expect(trackOverlayPayload('t1', [overlay({})], source)).toBeUndefined();
    }
  });
});
