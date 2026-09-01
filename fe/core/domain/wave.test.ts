import { describe, expect, it } from 'vitest';

import {
  activeWavesOn, createCardOperation, createCodexCardOperation, createTerminalCardOperation,
  deleteCardOperation, isRunning, isWaitingForUser, lifecycleLabel, toWave,
  NEUTRAL_ACTIVITY, UNTITLED_WAVE_LABEL, waveDisplayTitle, waveLifecycleSchema, waveWireSchema, wavesInCoveOperation,
  userVisibleWaves,
  type Wave,
} from './wave.js';
import type { Cove } from './cove.js';

const baseWire = {
  id: 'w1', cove_id: 'c1', title: 'Ship it', sort: 1,
  created_at: 1_000, updated_at: 1_000,
};

function wave(overrides: Partial<Wave>): Wave {
  return {
    id: 'w', coveId: 'c', title: 't', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

const DAY = 24 * 60 * 60 * 1000;

describe('wave wire decode', () => {
  it('fills the kernel serde defaults so the decoded wave has no optional fields', () => {
    const parsed = waveWireSchema.parse(baseWire);
    expect(parsed).toMatchObject({
      lifecycle: 'draft', cwd: '', archived_at: null, pinned_at: null, terminal_at: null,
    });
  });

  it('keeps explicit wire values over the defaults', () => {
    const parsed = waveWireSchema.parse({ ...baseWire, lifecycle: 'working', cwd: '/srv', terminal_at: 7 });
    expect(parsed.lifecycle).toBe('working');
    expect(parsed.cwd).toBe('/srv');
    expect(parsed.terminal_at).toBe(7);
  });

  it('rejects a lifecycle outside the kernel vocabulary', () => {
    expect(waveWireSchema.safeParse({ ...baseWire, lifecycle: 'archived' }).success).toBe(false);
  });

  it('drops server fields this slice does not model instead of failing the decode', () => {
    expect(waveWireSchema.safeParse({ ...baseWire, workflow_id: null, purpose: null }).success).toBe(true);
  });

  it('maps the wire row onto the camelCase domain shape', () => {
    expect(toWave(waveWireSchema.parse({ ...baseWire, pinned_at: 42 }))).toEqual(wave({
      id: 'w1', coveId: 'c1', title: 'Ship it', cwd: '', pinnedAt: 42, createdAt: 1_000, updatedAt: 1_000,
    }));
  });

  it('percent-encodes the cove id into the list path', () => {
    expect(wavesInCoveOperation('a/b').path).toBe('/api/coves/a%2Fb/waves');
  });
});

/*
 * The card writes, as requests.
 *
 * These three are the whole contract between the browser and the kernel for
 * adding and removing a card, and a wrong verb or a wrong path is a defect no
 * caller-side test can see: the mutation hooks report whatever the operation
 * says. The ids are percent-encoded because a wave id or a card id is an opaque
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
    expect(operation.path).toBe('/api/waves/w%2F1/codex-cards');
    expect(operation.body).toBe(body);
  });

  it('mints a terminal card on its own atomic endpoint, not the generic one', () => {
    const operation = createTerminalCardOperation('w1', { theme });
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/waves/w1/terminal-cards');
  });

  it('writes a runtime-less card through the generic create with its kind and payload', () => {
    const body = { kind: 'file-viewer', payload: { path: '/repo/notes.md' }, title: 'Notes' };
    const operation = createCardOperation('w/1', body);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/waves/w%2F1/cards');
    expect(operation.body).toBe(body);
  });
});

describe('lifecycle predicates', () => {
  it('splits the vocabulary into waiting, running, and quiet', () => {
    const waiting = waveLifecycleSchema.options.filter(isWaitingForUser);
    const running = waveLifecycleSchema.options.filter(isRunning);
    expect(waiting).toEqual(['blocked', 'reviewing', 'failed']);
    expect(running).toEqual(['planning', 'dispatching', 'working']);
    expect(waveLifecycleSchema.options.filter((l) => !isWaitingForUser(l) && !isRunning(l)))
      .toEqual(['draft', 'done', 'canceled']);
  });

  it('labels every lifecycle exactly once', () => {
    const labels = waveLifecycleSchema.options.map(lifecycleLabel);
    expect(new Set(labels).size).toBe(labels.length);
    expect(lifecycleLabel('reviewing')).toBe('In review');
  });

  it('falls back to a single untitled label', () => {
    expect(waveDisplayTitle('   ')).toBe(UNTITLED_WAVE_LABEL);
    expect(waveDisplayTitle(' Ship ')).toBe('Ship');
  });
});

describe('activeWavesOn', () => {
  const day = new Date(2026, 7, 10, 12, 0, 0);
  const dayStart = new Date(2026, 7, 10, 0, 0, 0).getTime();
  const dayEnd = new Date(2026, 7, 10, 23, 59, 59, 999).getTime();
  const now = day.getTime();

  it('includes an open wave created before the day and still running', () => {
    const open = wave({ id: 'open', createdAt: dayStart - DAY, terminalAt: null });
    expect(activeWavesOn([open], day, now).map((w) => w.id)).toEqual(['open']);
  });

  it('includes a wave created in the last millisecond of the day', () => {
    const late = wave({ id: 'late', createdAt: dayEnd, terminalAt: null });
    expect(activeWavesOn([late], day, now).map((w) => w.id)).toEqual(['late']);
  });

  it('includes a wave that ended exactly at the start of the day', () => {
    const edge = wave({ id: 'edge', createdAt: dayStart - DAY, terminalAt: dayStart });
    expect(activeWavesOn([edge], day, now).map((w) => w.id)).toEqual(['edge']);
  });

  it('excludes a wave that ended before the day and one created after it', () => {
    const before = wave({ id: 'before', createdAt: dayStart - 2 * DAY, terminalAt: dayStart - 1 });
    const after = wave({ id: 'after', createdAt: dayEnd + 1, terminalAt: null });
    expect(activeWavesOn([before, after], day, now)).toEqual([]);
  });

  it('uses updatedAt as the end of a terminal wave when terminalAt is absent', () => {
    const staleDone = wave({
      id: 'done-with-defaulted-terminal', lifecycle: 'done',
      createdAt: dayStart - 3 * DAY, updatedAt: dayStart - 2 * DAY, terminalAt: null,
    });
    expect(activeWavesOn([staleDone], day, now)).toEqual([]);
  });

  it('orders oldest first and breaks ties by id', () => {
    const b = wave({ id: 'b', createdAt: dayStart + 10 });
    const a = wave({ id: 'a', createdAt: dayStart + 10 });
    const older = wave({ id: 'z', createdAt: dayStart + 1 });
    expect(activeWavesOn([b, a, older], day, now).map((w) => w.id)).toEqual(['z', 'a', 'b']);
  });

  it('does not mutate the input list', () => {
    const list = [wave({ id: 'b', createdAt: dayStart + 2 }), wave({ id: 'a', createdAt: dayStart + 1 })];
    activeWavesOn(list, day, now);
    expect(list.map((w) => w.id)).toEqual(['b', 'a']);
  });
});

describe('userVisibleWaves', () => {
  const userCove: Cove = {
    id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0,
  };
  const systemCove: Cove = {
    id: 'sys', name: 'Kernel', color: '#000000', sort: 0, kind: 'system', createdAt: 0, updatedAt: 0,
  };
  const mine = wave({ id: 'w1', coveId: 'c1' });
  const scaffolding = wave({ id: 'w-sys', coveId: 'sys' });
  const archived = wave({ id: 'w2', coveId: 'c1', archivedAt: 1 });

  it('[E2E-INV-SHELL-003] drops waves hosted by the system cove', () => {
    // The wave itself is perfectly ordinary — not archived, user-shaped. Only
    // its cove disqualifies it, which is the case `visibleWaves` alone misses.
    expect(userVisibleWaves([mine, scaffolding], [userCove, systemCove]).map((w) => w.id))
      .toEqual(['w1']);
  });

  it('drops archived waves and waves whose cove is absent from the list', () => {
    expect(userVisibleWaves([mine, archived], [userCove]).map((w) => w.id)).toEqual(['w1']);
    expect(userVisibleWaves([mine], [])).toEqual([]);
  });
});
