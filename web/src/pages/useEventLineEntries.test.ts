import { describe, expect, it } from 'vitest';
import type { KernelOverlay, WireEvent } from '../api/wire';
import type { CodexCardData } from '../cards/builtins/codex';
import type { SpecCardData } from '../cards/builtins/spec';
import type { WaveCardSlot } from '../types';
import {
  createEventLineState,
  eventToLineEntry,
  filterEventLineEntriesForWave,
  reduceEventLineEntries,
  reduceEventLineState,
  selectAnyRuntimeLive,
  type EventLineEntry,
} from './useEventLineEntries';

type EventOf<K extends WireEvent['ev']> = Extract<WireEvent, { ev: K }>;

function reportEdited(
  author: EventOf<'wave.report_edited'>['data']['author'],
): EventOf<'wave.report_edited'> {
  return {
    ev: 'wave.report_edited',
    data: {
      wave_id: 'wave_1',
      card_id: 'report_1',
      author,
      edit_id: `edit_${author}`,
      summary_before: '',
      summary_after: '',
      body_before: '',
      body_after: 'body',
    },
  };
}

function cardAdded(
  kind: string,
  id: string,
  idempotencyKey?: string,
): EventOf<'card.added'> {
  return {
    ev: 'card.added',
    data: {
      id,
      wave_id: 'wave_1',
      kind,
      sort: 0,
      payload: idempotencyKey ? { idempotency_key: idempotencyKey } : {},
      deletable: true,
      created_at: 0,
      updated_at: 0,
    },
  };
}

function cardDeleted(id: string): EventOf<'card.deleted'> {
  return {
    ev: 'card.deleted',
    data: {
      id,
      wave_id: 'wave_1',
    },
  };
}

function taskFailed(key = 'task_1'): EventOf<'task.failed'> {
  return {
    ev: 'task.failed',
    data: {
      idempotency_key: key,
      reason: 'failed',
    },
  };
}

function runtimeStarted(cardId = 'card_1'): EventOf<'runtime.started'> {
  return {
    ev: 'runtime.started',
    data: {
      runtime_id: `runtime_${cardId}`,
      card_id: cardId,
      kind: 'codex',
      agent_provider: 'codex',
      status: 'running',
    },
  };
}

function runtimeFailed(cardId = 'card_1'): EventOf<'runtime.status_changed'> {
  return {
    ev: 'runtime.status_changed',
    data: {
      runtime_id: `runtime_${cardId}`,
      card_id: cardId,
      old_status: 'running',
      new_status: 'failed',
    },
  };
}

function workerSlot(
  cardId = 'card_1',
  idempotencyKey = 'task_1',
): WaveCardSlot {
  const card: CodexCardData = {
    type: 'codex',
    id: cardId,
    idempotencyKey,
  };
  return { kind: 'card', card };
}

function specSlot(cardId = 'spec_1'): WaveCardSlot {
  const card: SpecCardData = {
    type: 'spec',
    id: cardId,
  };
  return { kind: 'card', card };
}

function statusOverlay(cardId: string, state: string): KernelOverlay {
  return {
    id: `overlay_${cardId}`,
    plugin_id: 'kernel',
    entity_kind: 'card',
    entity_id: cardId,
    kind: 'status',
    payload: { state },
    updated_at: 0,
  };
}

function eventScope(
  waveId = 'wave_1',
  cards: WaveCardSlot[] = [workerSlot()],
) {
  return {
    waveId,
    cardIdSet: new Set(
      cards.flatMap((slot) =>
        slot.kind === 'card' && slot.card.id ? [slot.card.id] : [],
      ),
    ),
    idempotencyKeySet: new Set(
      cards.flatMap((slot) => {
        if (slot.kind !== 'card') return [];
        const key = (slot.card as { idempotencyKey?: string }).idempotencyKey;
        return key ? [key] : [];
      }),
    ),
  };
}

function harnessItem(): EventOf<'harness.item.added'> {
  return {
    ev: 'harness.item.added',
    data: {
      runtime_id: 'runtime_1',
      card_id: 'card_1',
      wave_id: 'wave_1',
      item_db_id: 1,
      item_uuid: 'item_1',
      item_type: 'tool_call',
      turn_id: 'turn_1',
      method: 'shell',
    },
  };
}

describe('eventToLineEntry', () => {
  it('keeps an empty event flow empty', () => {
    expect(reduceEventLineEntries([], { type: 'reset' })).toEqual([]);
  });

  it('maps wave.report_edited from an agent to an accent regeneration entry', () => {
    const entry = eventToLineEntry(reportEdited('spec'), 1_000);

    expect(entry).toMatchObject({
      title: 'Report regenerated',
      tag: 'agent',
      tone: 'accent',
      time: 1_000,
    });
  });

  it('maps wave.report_edited from a user to a default edit entry', () => {
    const entry = eventToLineEntry(reportEdited('user'), 1_000);

    expect(entry).toMatchObject({
      title: 'Report edited',
      tag: 'user',
      tone: 'default',
    });
  });

  it('silences kernel-minted wave-report card adds', () => {
    const entry = eventToLineEntry(cardAdded('wave-report', 'report_1'), 1_000);

    expect(entry).toBeNull();
  });

  it('silences kernel-minted spec card adds', () => {
    const entry = eventToLineEntry(cardAdded('spec', 'spec_1'), 1_000);

    expect(entry).toBeNull();
  });

  it('maps task.failed to an amber alert entry', () => {
    const entry = eventToLineEntry(taskFailed(), 1_000);

    expect(entry).toMatchObject({
      title: 'Task failed',
      tag: 'alert',
      tone: 'amber',
    });
  });

  it('aggregates adjacent entries with the same dedupe key inside the window', () => {
    let entries: EventLineEntry[] = [];
    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_1'), now: 1_000 },
      { dedupWindowMs: 30_000 },
    );
    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_1'), now: 20_000 },
      { dedupWindowMs: 30_000 },
    );

    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ title: 'Task failed', count: 2 });
    expect(entries[0].time).toBe(20_000);
  });

  it('does not aggregate task failures with different idempotency keys', () => {
    let entries: EventLineEntry[] = [];
    const scope = eventScope('wave_1', [
      workerSlot('card_1', 'task_1'),
      workerSlot('card_2', 'task_2'),
    ]);

    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_1'), now: 1_000, scope },
      { dedupWindowMs: 30_000 },
    );
    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_2'), now: 20_000, scope },
      { dedupWindowMs: 30_000 },
    );

    expect(entries).toHaveLength(2);
    expect(entries.map((entry) => entry.identityKey)).toEqual([
      'task:task.failed:task_2',
      'task:task.failed:task_1',
    ]);
  });

  it('aggregates duplicate task failures with the same idempotency key', () => {
    let entries: EventLineEntry[] = [];
    const scope = eventScope();

    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_1'), now: 1_000, scope },
      { dedupWindowMs: 30_000 },
    );
    entries = reduceEventLineEntries(
      entries,
      { type: 'event', ev: taskFailed('task_1'), now: 2_000, scope },
      { dedupWindowMs: 30_000 },
    );

    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      identityKey: 'task:task.failed:task_1',
      count: 2,
    });
  });

  it('drops card-scoped events for cards outside the current wave', () => {
    const entries = reduceEventLineEntries([], {
      type: 'event',
      ev: runtimeFailed('card_other'),
      now: 1_000,
      scope: eventScope('wave_1', [workerSlot('card_1', 'task_1')]),
    });

    expect(entries).toEqual([]);
  });

  it('drops task events whose idempotency key is not owned by current wave cards', () => {
    const entries = reduceEventLineEntries([], {
      type: 'event',
      ev: taskFailed('task_other'),
      now: 1_000,
      scope: eventScope('wave_1', [workerSlot('card_1', 'task_1')]),
    });

    expect(entries).toEqual([]);
  });

  it('keeps a new card runtime event that arrives before cards prop refetch', () => {
    let state = createEventLineState('wave_1', []);

    state = reduceEventLineState(state, {
      type: 'event',
      ev: cardAdded('codex', 'card_new'),
      now: 1_000,
    });
    state = reduceEventLineState(state, {
      type: 'event',
      ev: runtimeStarted('card_new'),
      now: 2_000,
    });

    expect(state.entries.map((entry) => entry.identityKey)).toContain(
      'runtime:runtime.started:runtime_card_new',
    );
  });

  it('drops card-scoped runtime events after the card is deleted', () => {
    let state = createEventLineState('wave_1', [
      workerSlot('card_deleted', 'task_deleted'),
    ]);

    state = reduceEventLineState(state, {
      type: 'event',
      ev: cardDeleted('card_deleted'),
      now: 1_000,
    });
    state = reduceEventLineState(state, {
      type: 'event',
      ev: runtimeFailed('card_deleted'),
      now: 2_000,
    });

    expect(state.entries.map((entry) => entry.title)).toEqual(['Card removed']);
    expect(state.entries).not.toContainEqual(
      expect.objectContaining({ identityKey: expect.stringContaining('runtime:') }),
    );
  });

  it('unions cards prop refetch scope without clobbering WS-added cards', () => {
    let state = createEventLineState('wave_1', []);

    state = reduceEventLineState(state, {
      type: 'event',
      ev: cardAdded('codex', 'card_1'),
      now: 1_000,
    });
    state = reduceEventLineState(state, {
      type: 'event',
      ev: runtimeStarted('card_1'),
      now: 2_000,
    });
    state = reduceEventLineState(state, {
      type: 'merge-scope',
      waveId: 'wave_1',
      cardIds: new Set(['card_1', 'card_2']),
      idempotencyKeys: new Set<string>(),
    });
    state = reduceEventLineState(state, {
      type: 'event',
      ev: runtimeStarted('card_2'),
      now: 3_000,
    });

    expect(state.entries.map((entry) => entry.identityKey)).toEqual([
      'runtime:runtime.started:runtime_card_2',
      'runtime:runtime.started:runtime_card_1',
      'card:card.added:card_1',
    ]);
  });

  it('keeps only entries for the active wave when filtering display entries', () => {
    const waveOneEntry = eventToLineEntry(
      cardAdded('worker', 'card_1'),
      1_000,
      'wave_1',
    );
    const waveTwoEntry = eventToLineEntry(
      cardAdded('worker', 'card_2'),
      2_000,
      'wave_2',
    );

    expect(waveOneEntry).not.toBeNull();
    expect(waveTwoEntry).not.toBeNull();

    const filtered = filterEventLineEntriesForWave(
      [waveTwoEntry, waveOneEntry].filter(
        (entry): entry is EventLineEntry => entry !== null,
      ),
      'wave_2',
    );

    expect(filtered).toHaveLength(1);
    expect(filtered[0].waveId).toBe('wave_2');
    expect(filtered[0].title).toBe('Worker added: worker');
  });

  it('caps retained entries at maxEntries and drops the oldest', () => {
    let entries: EventLineEntry[] = [];
    for (let i = 0; i < 45; i += 1) {
      entries = reduceEventLineEntries(
        entries,
        {
          type: 'event',
          ev: cardAdded(`worker-${i}`, `card_${i}`),
          now: 1_000 + i,
        },
        { maxEntries: 40 },
      );
    }

    expect(entries).toHaveLength(40);
    expect(entries[0].title).toBe('Worker added: worker-44');
    expect(entries[entries.length - 1].title).toBe('Worker added: worker-5');
    expect(entries.some((entry) => entry.title === 'Worker added: worker-0')).toBe(
      false,
    );
  });

  it('ignores silent harness item events', () => {
    const entries = reduceEventLineEntries([], {
      type: 'event',
      ev: harnessItem(),
      now: 1_000,
    });

    expect(entries).toEqual([]);
  });
});

describe('selectAnyRuntimeLive', () => {
  it('treats a starting worker as live', () => {
    expect(
      selectAnyRuntimeLive(
        [workerSlot('worker_1')],
        [statusOverlay('worker_1', 'Starting')],
      ),
    ).toBe(true);
  });

  it('treats a running worker with an idle spec as live', () => {
    expect(
      selectAnyRuntimeLive(
        [specSlot('spec_1'), workerSlot('worker_1')],
        [statusOverlay('spec_1', 'Idle'), statusOverlay('worker_1', 'Working')],
      ),
    ).toBe(true);
  });

  it('does not treat all-idle runtime cards as live', () => {
    expect(
      selectAnyRuntimeLive(
        [specSlot('spec_1'), workerSlot('worker_1')],
        [statusOverlay('spec_1', 'Idle'), statusOverlay('worker_1', 'Idle')],
      ),
    ).toBe(false);
  });

  it('treats a running worker as live when the spec card is absent', () => {
    expect(
      selectAnyRuntimeLive(
        [workerSlot('worker_1')],
        [statusOverlay('worker_1', 'Working')],
      ),
    ).toBe(true);
  });
});
