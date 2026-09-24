import { describe, expect, it } from 'vitest';

import {
  activeTracksOn, cardGoalTitle, createCardOperation, createCodexCardOperation, createTerminalCardOperation,
  createTrackOperation, deleteCardOperation, hasFailed, isBlankForKernel, isRunning, isWaitingForUser,
  isWorking, lifecycleLabel, lifecycleRank, needsUserAttention, toTrack, trackActivityFrom,
  sortAreaTracksByRecent, trackRecentAt,
  trackActivityState, trackDetailSchema, updateTrackOperation,
  NEUTRAL_ACTIVITY, UNTITLED_TRACK_LABEL, trackDisplayTitle, trackLifecycleSchema, trackWireSchema, tracksInAreaOperation,
  trackCreateKeyAction, userVisibleTracks, trackOverlayPayload,
  type Track, type OverlayWire,
} from './track.js';
import type { Area } from './area.js';

const baseWire = {
  id: 'w1', area_id: 'c1', title: 'Ship it', sort: 1,
  created_at: 1_000, updated_at: 1_000,
};

function track(overrides: Partial<Track>): Track {
  return {
    id: 'w', areaId: 'c', title: 't', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

const DAY = 24 * 60 * 60 * 1000;

describe('track wire decode', () => {
  it('fills the kernel serde defaults so the decoded track has no optional fields', () => {
    const parsed = trackWireSchema.parse(baseWire);
    expect(parsed).toMatchObject({
      lifecycle: 'draft', cwd: '', archived_at: null, pinned_at: null, terminal_at: null,
    });
  });

  it('keeps explicit wire values over the defaults', () => {
    const parsed = trackWireSchema.parse({ ...baseWire, lifecycle: 'working', cwd: '/srv', terminal_at: 7 });
    expect(parsed.lifecycle).toBe('working');
    expect(parsed.cwd).toBe('/srv');
    expect(parsed.terminal_at).toBe(7);
  });

  it('rejects a lifecycle outside the kernel vocabulary', () => {
    expect(trackWireSchema.safeParse({ ...baseWire, lifecycle: 'archived' }).success).toBe(false);
  });

  it('drops server fields this slice does not model instead of failing the decode', () => {
    expect(trackWireSchema.safeParse({ ...baseWire, template_id: null, purpose: null }).success).toBe(true);
  });

  it('maps the wire row onto the camelCase domain shape', () => {
    expect(toTrack(trackWireSchema.parse({ ...baseWire, pinned_at: 42 }))).toEqual(track({
      id: 'w1', areaId: 'c1', title: 'Ship it', cwd: '', pinnedAt: 42, createdAt: 1_000, updatedAt: 1_000,
    }));
  });

  it('percent-encodes the area id into the list path', () => {
    expect(tracksInAreaOperation('a/b').path).toBe('/api/areas/a%2Fb/tracks');
  });
});

describe('track create operation', () => {
  const theme = { fg: [1, 2, 3], bg: [4, 5, 6] } as const;

  it('carries the draft key exactly when a first message is present', () => {
    const keyed = createTrackOperation(
      { area_id: 'area', theme, first_message: 'ship it' },
      'draft-key',
    );
    expect(keyed.headers).toEqual({ 'Idempotency-Key': 'draft-key' });

    const messageLess = createTrackOperation({ area_id: 'area', theme });
    expect(messageLess.headers).toBeUndefined();
  });

  it('makes a message without a key invalid at both the type and runtime boundaries', () => {
    expect(() => {
      // @ts-expect-error first_message makes Idempotency-Key required.
      createTrackOperation({ area_id: 'area', theme, first_message: 'missing key' });
    }).toThrow(/Idempotency-Key/);
  });

  it('replaces only an explicitly exhausted key', () => {
    expect(trackCreateKeyAction({
      kind: 'http', status: 409, code: 'idempotency_key_exhausted', message: 'used up',
    })).toBe('replace');
    expect(trackCreateKeyAction({
      kind: 'http', status: 409, code: 'conflict', message: 'different payload',
    })).toBe('preserve');
    expect(trackCreateKeyAction({
      kind: 'http', status: 409, code: 'conflict', message: 'already used with different payload',
    })).toBe('offer-explicit-replace');
    expect(trackCreateKeyAction({
      kind: 'http', status: 409, code: 'conflict',
      message: 'this key predates durable request fingerprints',
    })).toBe('offer-explicit-replace');
    expect(trackCreateKeyAction({
      kind: 'http', status: 500, code: 'internal', message: 'unknown result',
    })).toBe('preserve');
    expect(trackCreateKeyAction({ kind: 'transport', message: 'answer lost' })).toBe('preserve');
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

describe('lifecycle predicates', () => {
  it('splits the vocabulary into waiting, running, and quiet', () => {
    const waiting = trackLifecycleSchema.options.filter(isWaitingForUser);
    const running = trackLifecycleSchema.options.filter(isRunning);
    expect(waiting).toEqual(['blocked', 'reviewing', 'failed']);
    expect(running).toEqual(['planning', 'dispatching', 'working']);
    expect(trackLifecycleSchema.options.filter((l) => !isWaitingForUser(l) && !isRunning(l)))
      .toEqual(['draft', 'done', 'canceled']);
  });

  it('labels every lifecycle exactly once', () => {
    const labels = trackLifecycleSchema.options.map(lifecycleLabel);
    expect(new Set(labels).size).toBe(labels.length);
    expect(lifecycleLabel('reviewing')).toBe('In review');
  });

  it('requires the server-derived Resume work capability on track detail', () => {
    const detail = { track: { ...baseWire }, cards: [], overlays: [] };
    expect(trackDetailSchema.safeParse(detail).success).toBe(false);
    expect(trackDetailSchema.parse({ ...detail, can_resume: true }).can_resume).toBe(true);
  });

  it('builds the lifecycle recovery PATCH without a parallel endpoint', () => {
    const operation = updateTrackOperation('w/1', { lifecycle: 'working' });
    expect(operation).toMatchObject({
      method: 'PATCH', path: '/api/tracks/w%2F1', body: { lifecycle: 'working' },
    });
  });

  it('falls back to a single untitled label', () => {
    expect(trackDisplayTitle('   ')).toBe(UNTITLED_TRACK_LABEL);
    expect(trackDisplayTitle(' Ship ')).toBe('Ship');
  });
});

describe('activity predicates read only the kernel activity overlay (INV-APP-118)', () => {
  it('does not derive attention from the lifecycle phase', () => {
    const reviewing = track({ lifecycle: 'reviewing', attention: 'none' });
    expect(needsUserAttention(reviewing)).toBe(false);
    expect(hasFailed(reviewing)).toBe(false);
    expect(trackActivityState(reviewing, false)).toBe('quiet');
    const blocked = track({ lifecycle: 'blocked', attention: 'none' });
    expect(needsUserAttention(blocked)).toBe(false);
    const failedPhase = track({ lifecycle: 'failed', attention: 'none' });
    expect(hasFailed(failedPhase)).toBe(false);
  });

  it('does not derive working from a running lifecycle phase', () => {
    expect(isWorking(track({ lifecycle: 'planning', working: false }))).toBe(false);
    expect(trackActivityState(track({ lifecycle: 'planning', working: false }), false)).toBe('quiet');
    expect(isWorking(track({ lifecycle: 'done', working: true }))).toBe(true);
    expect(trackActivityState(track({ lifecycle: 'done', working: true }), false)).toBe('working');
  });

  it('reads attention and failure from the overlay verdict', () => {
    expect(needsUserAttention(track({ lifecycle: 'draft', attention: 'input' }))).toBe(true);
    expect(hasFailed(track({ lifecycle: 'draft', attention: 'input' }))).toBe(false);
    expect(hasFailed(track({ lifecycle: 'done', attention: 'failed' }))).toBe(true);
    expect(needsUserAttention(track({ lifecycle: 'done', attention: 'failed' }))).toBe(false);
    expect(trackActivityState(track({ attention: 'input', working: true }), true)).toBe('attention');
    expect(trackActivityState(track({ attention: 'failed', working: true }), true)).toBe('failed');
    expect(trackActivityState(track({ attention: 'none', working: false }), true)).toBe('unread');
  });

  it('ranks a failed track with the waiting ones, and a running phase in the middle', () => {
    expect(lifecycleRank(track({ lifecycle: 'done', attention: 'failed' }))).toBe(0);
    expect(lifecycleRank(track({ lifecycle: 'done', attention: 'input' }))).toBe(0);
    expect(lifecycleRank(track({ lifecycle: 'planning', working: false }))).toBe(1);
    expect(lifecycleRank(track({ lifecycle: 'done', working: true }))).toBe(2);
    expect(lifecycleRank(track({ lifecycle: 'reviewing', attention: 'none' }))).toBe(2);
  });
});

describe('cardGoalTitle', () => {
  it('takes the first line of a string goal and nothing from any other payload', () => {
    expect(cardGoalTitle({ goal: 'Review the parser split\nThen post the verdict.' })).toBe('Review the parser split');
    expect(cardGoalTitle({ goal: '  Ship it  ' })).toBe('Ship it');
    expect(cardGoalTitle({ goal: '   ' })).toBeNull();
    /* The first line, not the first non-blank one: a goal whose first line is empty has no title. */
    expect(cardGoalTitle({ goal: '\nShip it' })).toBeNull();
    expect(cardGoalTitle({ goal: 42 })).toBeNull();
    expect(cardGoalTitle({ command: 'zsh' })).toBeNull();
    expect(cardGoalTitle({ planner_harness: true, prompt: 'Plan it' })).toBeNull();
    expect(cardGoalTitle(null)).toBeNull();
    expect(cardGoalTitle('goal')).toBeNull();
  });

  it('caps the title at 60 characters, counting code points', () => {
    const sixty = 'x'.repeat(60);
    expect(cardGoalTitle({ goal: sixty })).toBe(sixty);
    expect(cardGoalTitle({ goal: `${sixty}y` })).toBe(`${'x'.repeat(59)}…`);
    const cjk = '审'.repeat(61);
    expect([...cardGoalTitle({ goal: cjk })!]).toHaveLength(60);
    /* Astral characters are two UTF-16 units each: a `slice(0, 60)` on units keeps 30 whole characters,
     * but the impl-shaped `slice(0, 59) + '…'` stub keeps 29 plus a lone surrogate, so the cap counts
     * code points and never leaves a lone surrogate. */
    const astral = cardGoalTitle({ goal: '😀'.repeat(61) })!;
    expect([...astral]).toHaveLength(60);
    expect(astral).toBe(`${'😀'.repeat(59)}…`);
    expect(() => encodeURIComponent(astral)).not.toThrow();
  });
});

describe('trackActivityFrom: the kernel activity overlay', () => {
  const overlay = (payload: unknown, over: Partial<OverlayWire> = {}): OverlayWire => ({
    id: 'a1', plugin_id: 'kernel', entity_kind: 'track', entity_id: 't1', kind: 'activity',
    payload, updated_at: 1, ...over,
  });
  const payload = {
    schemaVersion: 1, working: true, attention: 'failed', activity_at_ms: 1_789_460_968_837,
    items: [
      { kind: 'failed', source: 'task', id: 'task-1', card_id: 'worker-1', at_ms: 20 },
      { kind: 'input', source: 'card', id: 'planner', card_id: 'planner', at_ms: 10 },
      { kind: 'failed', source: 'lifecycle', id: 't1', card_id: null, at_ms: 5 },
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
      { origin: 'task', id: 'task-1', cardId: 'worker-1', atMs: 20, kind: 'failed' },
      { origin: 'card', id: 'planner', cardId: 'planner', atMs: 10, kind: 'input' },
      { origin: 'lifecycle', id: 't1', cardId: null, atMs: 5, kind: 'failed' },
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
      items: [{ kind: 'input', source: 'card' }, ...payload.items],
      cards: [{ card_id: 'w9', state: 'sleeping' }, { card_id: 'w2', state: 'working' }],
    })]);
    expect(activity.attentionItems).toHaveLength(3);
    expect(activity.cards).toEqual({ w2: 'working' });
    expect(activity.working).toBe(true);
  });

  it('ignores a payload that is not the v1 shape at all', () => {
    for (const junk of [null, 'working', { schemaVersion: 2, working: true }, { working: 'yes' }]) {
      expect(trackActivityFrom('t1', [overlay(junk)])).toEqual({ ...NEUTRAL_ACTIVITY, recentAt: 1 });
    }
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

describe('activeTracksOn', () => {
  const day = new Date(2026, 7, 10, 12, 0, 0);
  const dayStart = new Date(2026, 7, 10, 0, 0, 0).getTime();
  const dayEnd = new Date(2026, 7, 10, 23, 59, 59, 999).getTime();
  const now = day.getTime();

  it('includes an open track created before the day and still running', () => {
    const open = track({ id: 'open', createdAt: dayStart - DAY, terminalAt: null });
    expect(activeTracksOn([open], day, now).map((w) => w.id)).toEqual(['open']);
  });

  it('includes a track created in the last millisecond of the day', () => {
    const late = track({ id: 'late', createdAt: dayEnd, terminalAt: null });
    expect(activeTracksOn([late], day, now).map((w) => w.id)).toEqual(['late']);
  });

  it('includes a track that ended exactly at the start of the day', () => {
    const edge = track({ id: 'edge', createdAt: dayStart - DAY, terminalAt: dayStart });
    expect(activeTracksOn([edge], day, now).map((w) => w.id)).toEqual(['edge']);
  });

  it('excludes a track that ended before the day and one created after it', () => {
    const before = track({ id: 'before', createdAt: dayStart - 2 * DAY, terminalAt: dayStart - 1 });
    const after = track({ id: 'after', createdAt: dayEnd + 1, terminalAt: null });
    expect(activeTracksOn([before, after], day, now)).toEqual([]);
  });

  it('uses updatedAt as the end of a terminal track when terminalAt is absent', () => {
    const staleDone = track({
      id: 'done-with-defaulted-terminal', lifecycle: 'done',
      createdAt: dayStart - 3 * DAY, updatedAt: dayStart - 2 * DAY, terminalAt: null,
    });
    expect(activeTracksOn([staleDone], day, now)).toEqual([]);
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
  const archived = track({ id: 'w2', areaId: 'c1', archivedAt: 1 });

  it('[E2E-INV-SHELL-003] drops tracks hosted by the system area', () => {
    expect(userVisibleTracks([mine, scaffolding], [userArea, systemArea]).map((w) => w.id))
      .toEqual(['w1']);
  });

  it('drops archived tracks and tracks whose area is absent from the list', () => {
    expect(userVisibleTracks([mine, archived], [userArea]).map((w) => w.id)).toEqual(['w1']);
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
