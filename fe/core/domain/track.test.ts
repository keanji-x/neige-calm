import { describe, expect, it } from 'vitest';

import {
  activeTracksOn, createCardOperation, createCodexCardOperation, createTerminalCardOperation,
  deleteCardOperation, isBlankForKernel, isRunning, isWaitingForUser, lifecycleLabel, toTrack,
  NEUTRAL_ACTIVITY, UNTITLED_TRACK_LABEL, trackDisplayTitle, trackLifecycleSchema, trackWireSchema, tracksInAreaOperation,
  userVisibleTracks,
  type Track,
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

/*
 * The card writes, as requests.
 *
 * These three are the whole contract between the browser and the kernel for
 * adding and removing a card, and a wrong verb or a wrong path is a defect no
 * caller-side test can see: the mutation hooks report whatever the operation
 * says. The ids are percent-encoded because a track id or a card id is an opaque
 * kernel string, and one containing `/` would otherwise address a different
 * route entirely.
 */
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

  it('falls back to a single untitled label', () => {
    expect(trackDisplayTitle('   ')).toBe(UNTITLED_TRACK_LABEL);
    expect(trackDisplayTitle(' Ship ')).toBe('Ship');
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
    // The track itself is perfectly ordinary — not archived, user-shaped. Only
    // its area disqualifies it, which is the case `visibleTracks` alone misses.
    expect(userVisibleTracks([mine, scaffolding], [userArea, systemArea]).map((w) => w.id))
      .toEqual(['w1']);
  });

  it('drops archived tracks and tracks whose area is absent from the list', () => {
    expect(userVisibleTracks([mine, archived], [userArea]).map((w) => w.id)).toEqual(['w1']);
    expect(userVisibleTracks([mine], [])).toEqual([]);
  });
});

/*
 * #1299 — the frontend's copy of the kernel's blank rule.
 *
 * The kernel refuses a first message whose `str::trim()` is empty, and Rust
 * trims on the Unicode `White_Space` property. This suite pins the two code
 * points where JS `trim()` and that property are known to differ in kind, and
 * pins the divergence itself rather than only the predicate: the second
 * assertion of the `U+0085` case is what says *why* this function exists, and
 * it is a live check of the platform, not a comment.
 */
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
    // The reason this predicate is not `text.trim() === ''`. A gate written
    // that way calls this string non-blank, enables the send, and posts a body
    // the kernel answers 400.
    expect('\u0085'.trim()).not.toBe('');
  });

  it('is false as soon as there is anything to say', () => {
    expect(isBlankForKernel('hi')).toBe(false);
    expect(isBlankForKernel('  keep indentation  ')).toBe(false);
    // Whitespace *around* content is content's neighbour, not blankness — and
    // the caller sends the string with it intact.
    expect(isBlankForKernel('\u0085x\u0085')).toBe(false);
  });
});
