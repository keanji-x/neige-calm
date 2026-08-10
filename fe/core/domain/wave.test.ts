import { describe, expect, it } from 'vitest';

import {
  activeWavesOn, isRunning, isWaitingForUser, lifecycleLabel, toWave,
  UNTITLED_WAVE_LABEL, waveDisplayTitle, waveLifecycleSchema, waveWireSchema, wavesInCoveOperation,
  type Wave,
} from './wave.js';

const baseWire = {
  id: 'w1', cove_id: 'c1', title: 'Ship it', sort: 1,
  created_at: 1_000, updated_at: 1_000,
};

function wave(overrides: Partial<Wave>): Wave {
  return {
    id: 'w', coveId: 'c', title: 't', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
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
