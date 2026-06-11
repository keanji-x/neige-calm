import { describe, expect, it } from 'vitest';
import type { WireEvent } from '../api/wire';
import {
  eventToLineEntry,
  reduceEventLineEntries,
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

function cardAdded(kind: string, id: string): EventOf<'card.added'> {
  return {
    ev: 'card.added',
    data: {
      id,
      wave_id: 'wave_1',
      kind,
      sort: 0,
      payload: {},
      deletable: true,
      created_at: 0,
      updated_at: 0,
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

  it('maps a wave-report card add to a report-created entry', () => {
    const entry = eventToLineEntry(cardAdded('wave-report', 'report_1'), 1_000);

    expect(entry).toMatchObject({
      title: 'Report created',
      tag: 'init',
      tone: 'accent',
    });
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
