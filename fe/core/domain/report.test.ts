import { describe, expect, it } from 'vitest';

import type { CardWire } from './track.js';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, groupBacklinks, hasLiveTaskRun,
  parseReportLink, readTrackReport, TASK_STATUS_DETAIL_LIMIT, TRACK_REPORT_CARD_KIND, trackTaskVerdictsOperation,
  type TaskVerdict, type TrackBacklink,
} from './report.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: {}, deletable: false, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

function prose(id: string, markdown: string) {
  return { id, kind: 'prose', rev: 1, payload: { markdown } };
}

describe('readTrackReport', () => {
  it('reads summary and body out of the track-report card', () => {
    const cards = [
      card({ id: 'other', kind: 'codex', payload: { body: 'not this one' } }),
      card({ payload: { schemaVersion: 3, docRev: 7, summary: 'One line', body: '# Goal\n\nDo the thing.' } }),
    ];
    expect(readTrackReport(cards)).toEqual({ summary: 'One line', body: '# Goal\n\nDo the thing.', blocks: null });
  });

  it('keeps a report whose summary is written while its body is blank', () => {
    expect(readTrackReport([card({ payload: { summary: 'Agent finished the migration.', body: '  ' } })]))
      .toEqual({ summary: 'Agent finished the migration.', body: '', blocks: null });
  });

  it('ignores fields it does not render, including ones it has never seen', () => {
    const cards = [card({ payload: { schemaVersion: 9, docRev: 1, summary: 's', body: 'b', future: {} } })];
    expect(readTrackReport(cards)?.body).toBe('b');
  });

  it.each([
    ['no report card at all', [card({ kind: 'codex' })]],
    ['a payload that is not an object', [card({ payload: 'nope' })]],
    ['the untouched payload a fresh track carries', [card({ payload: {} })]],
    ['a body that is only whitespace', [card({ payload: { body: '   \n  ' } })]],
    ['a blank body next to an empty blocks array', [card({ payload: { body: '', blocks: [] } })]],
  ])('reads null for %s', (_label, cards) => {
    expect(readTrackReport(cards as readonly CardWire[])).toBeNull();
  });

  it('does not distinguish a fresh track from an unreadable payload', () => {
    expect(readTrackReport([card({ payload: {} })]))
      .toEqual(readTrackReport([card({ payload: 42 })]));
  });

  it('reads the typed blocks, which are what carries each block id', () => {
    const report = readTrackReport([card({
      payload: {
        body: '# Goal',
        blocks: [
          prose('b-1', '# Goal'),
          { id: 'b-2', kind: 'table', rev: 3, payload: { columns: [{ key: 'k', label: 'K' }], rows: [{ k: 'v' }] } },
        ],
      },
    })]);
    expect(report?.blocks?.map((block) => [block.id, block.kind]))
      .toEqual([['b-1', 'prose'], ['b-2', 'table']]);
  });

  it('normalizes a persisted terminal goal into the discriminated command field', () => {
    const report = readTrackReport([card({
      payload: {
        schemaVersion: 3, docRev: 4, summary: 'legacy', body: 'legacy terminal',
        blocks: [{
          id: 'b-terminal', kind: 'task', rev: 7,
          payload: {
            key: 'compile', kind: 'terminal', goal: 'cargo check',
            ready: true, declared_by: 's\u0070ec',
          },
        }],
      },
    })]);

    const block = report?.blocks?.[0];
    expect(block).toMatchObject({
      id: 'b-terminal', kind: 'task',
      payload: { key: 'compile', kind: 'terminal', command: 'cargo check' },
    });
    if (block?.kind !== 'task') throw new Error('expected migrated task block');
    expect(block.payload).not.toHaveProperty('goal');
  });

  it('drops one wire-invalid block while keeping the other blocks', () => {
    const report = readTrackReport([card({
      payload: {
        body: '# Goal',
        blocks: [prose('b-1', '# Goal'), { id: '', kind: 'prose', payload: {} }, prose('b-2', 'Still here.')],
      },
    })]);
    expect(report?.blocks?.map((block) => block.id)).toEqual(['b-1', 'b-2']);
  });

  it('falls back to body when blocks is not an array', () => {
    expect(readTrackReport([card({ payload: { body: '# Goal', blocks: 'nope' } })]))
      .toEqual({ summary: '', body: '# Goal', blocks: null });
  });

  it('keeps a task live when an absent optional field is explicit null', () => {
    const report = readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [{
          id: 'b-1', kind: 'task', rev: 1,
          payload: {
            key: 'task-1', kind: 'codex', goal: 'Fix it', ready: true,
            declared_by: 'spec', spawn: null,
          },
        }],
      },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('task');
  });

  it('accepts a 2048-code-point string even when emoji use two UTF-16 code units', () => {
    const src = `/${'😀'.repeat(2047)}`;
    const report = readTrackReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('app');
  });

  it.each([
    ['a kind this build has never seen', { id: 'b-1', kind: 'chart.sankey', rev: 1, payload: { nodes: [] } }],
    ['a known kind whose payload does not fit', { id: 'b-1', kind: 'table', rev: 1, payload: { columns: [] } }],
    ['a known kind whose payload is not an object', { id: 'b-1', kind: 'prose', rev: 1, payload: 7 }],
  ])('degrades %s to one unsupported block, keeping the others', (_label, bad) => {
    const report = readTrackReport([card({
      payload: { body: 'x', blocks: [bad, prose('b-2', 'Still here.')] },
    })]);
    expect(report?.blocks?.[0]).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: bad.kind });
    expect(report?.blocks?.[1]?.kind).toBe('prose');
  });

  it.each([
    ['a protocol-relative path', '//evil.example/x'],
    ['a backslash the browser would normalize to a slash', '/\\evil.example/x'],
    ['an absolute URL', 'https://evil.example/x'],
    ['a control character', '/apps/a\0b'],
  ])('refuses an app block whose src is %s', (_label, src) => {
    const report = readTrackReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('unsupported');
  });
});

describe('deriveReportTasks', () => {
  function tasksOf(blocks: unknown[], verdicts?: readonly TaskVerdict[]) {
    return deriveReportTasks(
      readTrackReport([card({ payload: { body: 'x', blocks } })])?.blocks ?? null,
      verdicts,
    );
  }

  function task(id: string, payload: Record<string, unknown>) {
    return { id, kind: 'task', rev: 1, payload };
  }

  const live = (key: string, ready: boolean) =>
    ({ key, kind: 'codex', declared_by: 'spec', ready, goal: 'g' });

  function verdict(overrides: Partial<TaskVerdict> & Pick<TaskVerdict, 'blockId' | 'key'>): TaskVerdict {
    return { schedulable: true, status: null, statusDetail: null, workerCardId: null, ...overrides };
  }

  it('lists every task in document order and nothing else', () => {
    expect(tasksOf([
      prose('b-1', '# One\n'),
      task('b-2', live('alpha', true)),
      { id: 'b-3', kind: 'table', rev: 1, payload: { caption: 'c', columns: [], rows: [] } },
      task('b-4', live('beta', false)),
    ])).toEqual([
      { blockId: 'b-2', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
      { blockId: 'b-4', key: 'beta', state: 'not-ready', declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
    ]);
  });

  /* A live task may carry an explicit `tombstone: null`, so the presence of that key proves nothing. */
  it('reads a task carrying an explicit null tombstone as live, not withdrawn', () => {
    expect(tasksOf([task('b-1', { ...live('alpha', true), tombstone: null })]))
      .toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null }]);
  });

  it('keeps a withdrawn task', () => {
    expect(tasksOf([task('b-1', {
      key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: { reason: 'r' },
    })])).toEqual([{
      blockId: 'b-1', key: 'gone', state: 'withdrawn',
      declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
    }]);
  });

  it('keeps a task block whose payload does not parse, named by its id', () => {
    expect(tasksOf([task('b-1', { key: 'broken' })])).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
    }]);
  });

  it.each([
    ['withdrawn', { key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }],
    ['unreadable', { key: 'broken' }],
  ])('gives a %s row no worker kind, so it can offer no card', (_label, payload) => {
    const row = tasksOf(
      [task('b-1', payload)],
      [verdict({ blockId: 'b-1', key: 'gone', status: 'running', workerCardId: 'card-9' })],
    )[0];
    expect(row?.kind).toBeNull();
    expect(row?.workerCardId).toBeNull();
  });

  it('falls back to the block id when the task declared an empty key', () => {
    expect(tasksOf([task('b-1', live('', true))]))
      .toEqual([{ blockId: 'b-1', key: 'b-1', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null }]);
  });

  it('does not claim an unsupported block that declared some other kind', () => {
    expect(tasksOf([{ id: 'b-1', kind: 'chart.sankey', rev: 1, payload: {} }])).toEqual([]);
  });

  it('has no rows for a report with no blocks', () => {
    expect(deriveReportTasks(null)).toEqual([]);
  });

  it('reads a dispatched task as its status and its kind, and carries the worker card', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready',
      declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9', pendingReason: null,
    }]);
  });

  it.each([
    ['claude', 'verifying'],
    ['terminal', 'done'],
    ['codex', 'canceled'],
  ])('carries a %s worker\'s %s status and its kind as two facts', (kind, status) => {
    expect(tasksOf(
      [task('b-1', { ...live('alpha', true), kind })],
      [verdict({ blockId: 'b-1', key: 'alpha', status, workerCardId: 'card-9' })],
    )[0]).toMatchObject({ status, kind, workerCardId: 'card-9', declaration: null });
  });

  it('carries failed and the kind together, and keeps the worker card', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready',
      declaration: null, status: 'failed', statusDetail: null, kind: 'codex', workerCardId: 'card-9', pendingReason: null,
    }]);
  });

  it('carries the kernel\'s reason for a status alongside the status word', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({
        blockId: 'b-1', key: 'alpha', status: 'failed',
        statusDetail: 'track 9a4c is not a git repository', workerCardId: 'card-9',
      })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: 'failed',
      statusDetail: 'track 9a4c is not a git repository', kind: 'codex', workerCardId: 'card-9', pendingReason: null,
    }]);
  });

  it('drops the reason when the verdict carries no status to qualify', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: null, statusDetail: 'never dispatched' })],
    )[0]).toMatchObject({ status: null, statusDetail: null, declaration: null });
  });

  it.each([
    ['withdrawn', { key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }],
    ['unreadable', { key: 'broken' }],
  ])('gives a %s row no reason either, not just no status', (_label, payload) => {
    const row = tasksOf(
      [task('b-1', payload)],
      [verdict({
        blockId: 'b-1', key: 'gone', status: 'failed',
        statusDetail: 'track 9a4c is not a git repository', workerCardId: 'card-9',
      })],
    )[0];
    expect(row?.status).toBeNull();
    expect(row?.statusDetail).toBeNull();
  });

  it('gives neither row of a contested key a reason', () => {
    const rows = tasksOf(
      [task('b-one', live('alpha', true)), task('b-two', live('alpha', true))],
      [
        verdict({ blockId: 'b-one', key: 'alpha', status: null, statusDetail: 'boom' }),
        verdict({ blockId: 'b-two', key: 'alpha', status: null, statusDetail: 'boom' }),
      ],
    );
    expect(rows.map((row) => row.statusDetail)).toEqual([null, null]);
  });

  it.each([['empty', ''], ['whitespace only', '   \n  ']])('reads a %s reason as none', (_label, detail) => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', statusDetail: detail })],
    )[0]?.statusDetail).toBeNull();
  });

  it('collapses a multi-line reason onto one line', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({
        blockId: 'b-1', key: 'alpha', status: 'failed',
        statusDetail: '  spawn failed:\n  fatal: not a git repository\n',
      })],
    )[0]?.statusDetail).toBe('spawn failed: fatal: not a git repository');
  });

  it('bounds a reason longer than the limit and marks the elision', () => {
    const detail = tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', statusDetail: 'x'.repeat(400) })],
    )[0]?.statusDetail;
    expect(detail).toHaveLength(TASK_STATUS_DETAIL_LIMIT);
    expect(detail?.endsWith('…')).toBe(true);
  });

  /* The bound counts UTF-16 code units; `😀` is a surrogate pair placed to straddle the cut. */
  it('never cuts a reason in the middle of an astral character', () => {
    const detail = tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({
        blockId: 'b-1', key: 'alpha', status: 'failed',
        statusDetail: `${'x'.repeat(TASK_STATUS_DETAIL_LIMIT - 2)}😀…`,
      })],
    )[0]?.statusDetail ?? '';
    expect(`${'x'.repeat(TASK_STATUS_DETAIL_LIMIT - 2)}😀…`.length)
      .toBeGreaterThan(TASK_STATUS_DETAIL_LIMIT);
    expect([...detail].some((point) => {
      const code = point.codePointAt(0) ?? 0;
      return code >= 0xd800 && code <= 0xdfff;
    })).toBe(false);
    expect(detail.length).toBeLessThanOrEqual(TASK_STATUS_DETAIL_LIMIT);
    expect(detail.endsWith('…')).toBe(true);
  });

  it('leaves a reason exactly at the limit untouched', () => {
    const whole = 'y'.repeat(TASK_STATUS_DETAIL_LIMIT);
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', statusDetail: whole })],
    )[0]?.statusDetail).toBe(whole);
  });

  it('prints pending for an unassigned pending task whether or not it is schedulable', () => {
    const rows = tasksOf(
      [task('b-1', live('alpha', true)), task('b-2', live('beta', true))],
      [
        verdict({ blockId: 'b-1', key: 'alpha', status: 'pending', schedulable: true }),
        verdict({ blockId: 'b-2', key: 'beta', status: 'pending', schedulable: false }),
      ],
    );
    expect(rows.map((row) => row.status)).toEqual(['pending', 'pending']);
    expect(rows.every((row) => row.workerCardId === null)).toBe(true);
    expect(rows.every((row) => row.pendingReason === null)).toBe(true);
  });

  it('prints an unassigned status that is neither pending nor failed as it stands', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'done' })],
    )[0]?.status).toBe('done');
  });

  it('keeps the declaration word for a verdict that carries no status at all', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', schedulable: false })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'not-ready',
      declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
    }]);
  });

  it('drops the readiness word once the kernel reports a run', () => {
    const row = tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )[0];
    expect(row?.declaration).toBeNull();
    expect(row?.state).toBe('not-ready');
    expect(row?.status).toBe('running');
  });

  it('renders the declaration-only list when no verdicts have arrived', () => {
    const row = tasksOf([task('b-1', live('alpha', true))])[0];
    expect(row?.status).toBeNull();
    expect(row?.declaration).toBeNull();
    expect(row?.kind).toBe('codex');
  });

  it('drops a verdict whose key and block id match nothing in the report', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-99', key: 'ghost', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null }]);
  });

  it('leaves a task with no verdict alone while its neighbour runs', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true)), task('b-2', live('beta', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    ).map((row) => [row.status, row.kind])).toEqual([['running', 'codex'], [null, 'codex']]);
  });

  it('joins by block id first and falls back to the key', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-other', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )[0]?.status).toBe('running');
  });

  it('never matches an unreadable task through the block id standing in for its key', () => {
    expect(tasksOf(
      [task('b-1', { key: 'broken' })],
      [verdict({ blockId: 'b-other', key: 'b-1', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
    }]);
  });

  it('keeps Unreadable even when a verdict names the very block, and offers no card', () => {
    expect(tasksOf(
      [task('b-1', { key: 'broken' })],
      [verdict({ blockId: 'b-1', key: 'broken', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
    }]);
  });

  it('keeps the declaration word and no card for a withdrawn task that had already run', () => {
    expect(tasksOf(
      [task('b-1', { key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} })],
      [verdict({ blockId: 'b-1', key: 'gone', status: 'done', workerCardId: 'card-9', schedulable: false })],
    )).toEqual([{
      blockId: 'b-1', key: 'gone', state: 'withdrawn',
      declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
    }]);
  });

  it('reports a redeclared key on the live block only, never on the withdrawn one', () => {
    expect(tasksOf(
      [
        task('b-old', { key: 'alpha', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }),
        task('b-new', live('alpha', true)),
      ],
      [
        verdict({ blockId: 'b-old', key: 'alpha', status: 'running', workerCardId: 'card-9' }),
        verdict({ blockId: 'b-new', key: 'alpha', status: 'running', workerCardId: 'card-9' }),
      ],
    )).toEqual([
      { blockId: 'b-old', key: 'alpha', state: 'withdrawn', declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null },
      { blockId: 'b-new', key: 'alpha', state: 'ready', declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9', pendingReason: null },
    ]);
  });

  it('reports no run on either row when two live blocks claim the same key', () => {
    expect(tasksOf(
      [task('b-one', live('alpha', true)), task('b-two', live('alpha', false))],
      [
        verdict({ blockId: 'b-one', key: 'alpha', status: null, workerCardId: null }),
        verdict({ blockId: 'b-two', key: 'alpha', status: null, workerCardId: null }),
      ],
    )).toEqual([
      { blockId: 'b-one', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
      {
        blockId: 'b-two', key: 'alpha', state: 'not-ready',
        declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
      },
    ]);
  });

  it('keeps a run on the block the kernel gave it to when the verdicts lag the blocks', () => {
    expect(tasksOf(
      [task('b-one', live('alpha', true)), task('b-two', live('alpha', true))],
      [verdict({ blockId: 'b-one', key: 'alpha', status: 'done', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-one', 'done', 'card-1'],
      ['b-two', null, null],
    ]);
  });

  it('gives no row a run through the key when two rows claim that key', () => {
    expect(tasksOf(
      [task('b-two', live('alpha', true)), task('b-three', live('alpha', false))],
      [verdict({ blockId: 'b-gone', key: 'alpha', status: 'done', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId, row.declaration])).toEqual([
      ['b-two', null, null, null],
      ['b-three', null, null, 'Not ready'],
    ]);
  });

  it('gives no row a run through the key when a tombstone and its redeclaration share it', () => {
    expect(tasksOf(
      [
        task('b-old', { key: 'alpha', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }),
        task('b-new', live('alpha', true)),
      ],
      [verdict({ blockId: 'b-gone', key: 'alpha', status: 'done', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-old', null, null],
      ['b-new', null, null],
    ]);
  });

  it('leaves the sole live claimant blank by design when a tombstone shares its key', () => {
    expect(tasksOf(
      [
        task('b-old', { key: 'alpha', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }),
        task('b-new', live('alpha', true)),
      ],
      [verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-old', null, null],
      ['b-new', null, null],
    ]);
  });

  it('still falls back to the key when exactly one row claims it', () => {
    expect(tasksOf(
      [task('b-new', live('alpha', true)), task('b-other', live('beta', true))],
      [verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-new', 'running', 'card-1'],
      ['b-other', null, null],
    ]);
  });

  it('still reports the run of an uncontested key beside a contested one', () => {
    expect(tasksOf(
      [
        task('b-one', live('alpha', true)),
        task('b-two', live('alpha', true)),
        task('b-solo', live('beta', true)),
      ],
      [
        verdict({ blockId: 'b-one', key: 'alpha', status: null, workerCardId: null }),
        verdict({ blockId: 'b-two', key: 'alpha', status: null, workerCardId: null }),
        verdict({ blockId: 'b-solo', key: 'beta', status: 'running', workerCardId: 'card-7' }),
      ],
    ).map((row) => [row.status, row.workerCardId])).toEqual([
      [null, null],
      [null, null],
      ['running', 'card-7'],
    ]);
  });

  it('carries no run on either row when two live blocks both leave the key empty', () => {
    expect(tasksOf(
      [task('b-one', live('', true)), task('b-two', live('', true))],
      [
        verdict({ blockId: 'b-one', key: '', status: null, workerCardId: null }),
        verdict({ blockId: 'b-two', key: '', status: null, workerCardId: null }),
      ],
    ).map((row) => [row.key, row.status, row.workerCardId])).toEqual([
      ['b-one', null, null],
      ['b-two', null, null],
    ]);
  });

  it('still reports the run of a lone block that declared no key', () => {
    expect(tasksOf(
      [task('b-one', live('', true))],
      [verdict({ blockId: 'b-one', key: '', status: 'running', workerCardId: 'card-9' })],
    ).map((row) => [row.key, row.status, row.workerCardId])).toEqual([
      ['b-one', 'running', 'card-9'],
    ]);
  });

  it('treats an empty status as no run at all, reason and declaration word included', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: '', statusDetail: 'boom', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'not-ready', declaration: 'Not ready',
      status: null, statusDetail: null, kind: 'codex', workerCardId: 'card-9', pendingReason: null,
    }]);
  });

  it('never lets a verdict naming another report block reach a row through the key index', () => {
    expect(tasksOf(
      [task('b-alpha', live('alpha', true)), task('b-beta', live('beta', true))],
      [verdict({ blockId: 'b-beta', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([
      { blockId: 'b-alpha', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
      { blockId: 'b-beta', key: 'beta', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
    ]);
  });

  it('still reaches a row by key when the verdict names a block this report does not have', () => {
    expect(tasksOf(
      [task('b-alpha', live('alpha', true)), task('b-beta', live('beta', true))],
      [verdict({ blockId: 'b-stale', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([
      { blockId: 'b-alpha', key: 'alpha', state: 'ready', declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9', pendingReason: null },
      { blockId: 'b-beta', key: 'beta', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null },
    ]);
  });

  it('ignores a block-id hit whose key contradicts the block declaration', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'beta', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null }]);
  });

  it('stops at a contradicting block-id hit by design and never retries through the key index', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [
        verdict({ blockId: 'b-1', key: 'beta', status: 'done', workerCardId: 'card-8' }),
        verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-9' }),
      ],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null,
      status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
    }]);
  });

  it('treats an empty worker card id as no card at all', () => {
    const row = tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'pending', workerCardId: '' })],
    )[0];
    expect(row?.workerCardId).toBeNull();
    expect(row?.status).toBe('pending');
  });
});

describe('hasLiveTaskRun', () => {
  const blocksOf = (blocks: unknown[]) =>
    readTrackReport([card({ payload: { body: 'x', blocks } })])?.blocks ?? null;
  const declared = (id: string, key: string) =>
    ({ id, kind: 'task', rev: 1, payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' } });
  const rowsFor = (verdicts: TaskVerdict[], blocks: unknown[] = [declared('b-1', 'k')]) =>
    deriveReportTasks(blocksOf(blocks), verdicts);
  const at = (status: string | null) => rowsFor([{
    blockId: 'b-1', key: 'k', schedulable: true, status, workerCardId: null,
  }]);

  it('decorates the row it is asked about', () => {
    expect(at('running').map((row) => row.status)).toEqual(['running']);
  });

  it('is true inside the eventless mark_running window', () => {
    for (const status of ['dispatched', 'running']) {
      expect(hasLiveTaskRun(at(status))).toBe(true);
    }
  });

  it('is false for every terminal status', () => {
    for (const status of ['done', 'failed', 'canceled']) {
      expect(hasLiveTaskRun(at(status))).toBe(false);
    }
  });

  it('is false for a track holding nothing but pending rows, so a stuck track does not poll', () => {
    expect(hasLiveTaskRun(at('pending'))).toBe(false);
    expect(hasLiveTaskRun(rowsFor([
      { blockId: 'b-1', key: 'a', schedulable: false, status: 'pending', workerCardId: null },
      { blockId: 'b-2', key: 'b', schedulable: true, status: 'pending', workerCardId: null },
      { blockId: 'b-3', key: 'c', schedulable: false, status: 'canceled', workerCardId: null },
    ], [declared('b-1', 'a'), declared('b-2', 'b'), declared('b-3', 'c')]))).toBe(false);
  });

  it('is false while a gate verifies, which is evented on both sides', () => {
    expect(hasLiveTaskRun(at('verifying'))).toBe(false);
  });

  it('is false when a task has no status at all', () => {
    expect(hasLiveTaskRun(at(null))).toBe(false);
    expect(hasLiveTaskRun([])).toBe(false);
    expect(hasLiveTaskRun(undefined)).toBe(false);
  });

  it('is false for a status this build does not know, so an unknown word cannot poll forever', () => {
    expect(hasLiveTaskRun(at('quiescent'))).toBe(false);
  });

  it('is true when any one row is live', () => {
    expect(hasLiveTaskRun(rowsFor([
      { blockId: 'b-1', key: 'a', schedulable: true, status: 'done', workerCardId: 'c1' },
      { blockId: 'b-2', key: 'b', schedulable: true, status: 'running', workerCardId: 'c2' },
    ], [declared('b-1', 'a'), declared('b-2', 'b')]))).toBe(true);
  });

  it('is false for a live verdict whose declaration was deleted from the report', () => {
    const verdicts: TaskVerdict[] = [{
      blockId: '', key: 'deleted-task', schedulable: true, status: 'running', workerCardId: 'c-9',
    }];
    const rows = rowsFor(verdicts);
    expect(rows.map((row) => [row.key, row.status])).toEqual([['k', null]]);
    expect(hasLiveTaskRun(rows)).toBe(false);
  });

  it('is false for a live run on a key two live declarations both claim', () => {
    const rows = rowsFor([
      { blockId: 'b-1', key: 'dup', schedulable: true, status: null, workerCardId: null },
      { blockId: 'b-2', key: 'dup', schedulable: true, status: null, workerCardId: null },
    ], [declared('b-1', 'dup'), declared('b-2', 'dup')]);
    expect(rows.map((row) => row.status)).toEqual([null, null]);
    expect(hasLiveTaskRun(rows)).toBe(false);
  });
});

describe('trackTaskVerdictsOperation', () => {
  it('GETs the track report route with the id escaped', () => {
    const operation = trackTaskVerdictsOperation('w/1');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/tracks/w%2F1/report');
  });

  it('reads only the task diagnostics out of the response', () => {
    expect(trackTaskVerdictsOperation('w1').responseSchema.parse({
      schemaVersion: 3, docRev: 9, summary: 's', body: 'b', blocks: [{ id: 'b-1' }],
      taskDiagnostics: [{
        blockId: 'b-1', key: 'alpha', schedulable: true, status: 'running',
        workerCardId: 'card-9', gateResult: null, diagnostics: [],
      }],
    })).toEqual([{
      blockId: 'b-1', key: 'alpha', schedulable: true, status: 'running', workerCardId: 'card-9',
    }]);
  });

  it('reads the kernel\'s status detail off the verdict', () => {
    expect(trackTaskVerdictsOperation('w1').responseSchema.parse({
      taskDiagnostics: [{
        blockId: 'b-1', key: 'alpha', schedulable: true, status: 'failed',
        statusDetail: 'track 9a4c is not a git repository', diagnostics: [],
      }],
    })).toEqual([{
      blockId: 'b-1', key: 'alpha', schedulable: true, status: 'failed',
      statusDetail: 'track 9a4c is not a git repository',
    }]);
  });

  it('reads the server-owned pending diagnosis without re-deriving it', () => {
    const reason = {
      kind: 'budgetQueued' as const,
      message: 'Queued 1/1 — wait for a slot or raise task_budget',
      occupiedTaskBudget: 1,
      effectiveTaskBudget: 1,
    };
    expect(trackTaskVerdictsOperation('w1').responseSchema.parse({
      taskDiagnostics: [{
        blockId: 'b-1', key: 'alpha', schedulable: true, status: 'pending',
        pendingReason: reason, diagnostics: [],
      }],
    })[0]?.pendingReason).toEqual(reason);
  });

  it('drops a malformed verdict and keeps the rest', () => {
    expect(trackTaskVerdictsOperation('w1').responseSchema.parse({
      taskDiagnostics: [
        { blockId: 'b-1', key: 'alpha', schedulable: 'yes' },
        { blockId: 'b-2', key: 'beta', schedulable: false },
      ],
    })).toEqual([{ blockId: 'b-2', key: 'beta', schedulable: false }]);
  });

  it('reads an absent taskDiagnostics as no verdicts', () => {
    expect(trackTaskVerdictsOperation('w1').responseSchema.parse({ summary: 's' })).toEqual([]);
  });
});

describe('deriveReportOutline', () => {
  it('keeps H1 sections at the top level and hangs H2 headings beneath them', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [prose('b-1', '# One\n\ntext\n\n## Two\n'), prose('b-2', '# Three\n')],
      },
    })])?.blocks ?? null);
    expect(outline.map((item) => [item.number, item.label, item.blockId])).toEqual([
      [1, 'One', 'b-1-h1'],
      [2, 'Three', 'b-2-h1'],
    ]);
    expect(outline[0]?.children).toEqual([{ blockId: 'b-1-h2', label: 'Two' }]);
  });

  it('hangs a non-prose block under the section above it, as evidence rather than a section', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [
          prose('b-1', '# Valuation\n'),
          { id: 'b-2', kind: 'chart.candles', rev: 1, payload: { symbol: '600519', candles: [[0, 1, 2, 0, 1], [1, 1, 2, 0, 1]] } },
        ],
      },
    })])?.blocks ?? null);
    expect(outline).toHaveLength(1);
    expect(outline[0]?.children).toEqual([{ blockId: 'b-2', label: '600519' }]);
  });

  it('leaves task blocks out: they are not in the document flow any more', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [
          prose('b-1', '# Valuation\n'),
          { id: 'b-2', kind: 'task', rev: 1, payload: { key: 'alpha', kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' } },
          { id: 'b-3', kind: 'table', rev: 1, payload: { caption: 'Comparables', columns: [{ key: 'k', label: 'K' }], rows: [] } },
        ],
      },
    })])?.blocks ?? null);
    expect(outline).toHaveLength(1);
    expect(outline[0]?.children).toEqual([{ blockId: 'b-3', label: 'Comparables' }]);
  });

  it('leaves out a task whose payload did not parse, which degrades to unsupported', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [
          prose('b-1', '# Valuation\n'),
          { id: 'b-2', kind: 'task', rev: 1, payload: { key: 'broken' } },
        ],
      },
    })])?.blocks ?? null);
    expect(outline).toHaveLength(1);
    expect(outline[0]?.children).toEqual([]);
  });

  it('promotes a leading non-prose block to an unnumbered top-level item', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: {
        body: 'x',
        blocks: [
          { id: 'b-1', kind: 'table', rev: 1, payload: { columns: [{ key: 'k', label: 'K' }], rows: [], caption: 'Comparables' } },
          prose('b-2', '# After\n'),
        ],
      },
    })])?.blocks ?? null);
    expect(outline.map((item) => [item.number, item.label])).toEqual([
      [null, 'Comparables'],
      [1, 'After'],
    ]);
  });

  it('keeps H2 headings navigable when the report has no preceding H1 and ignores H3', () => {
    const outline = deriveReportOutline(readTrackReport([card({
      payload: { body: 'x', blocks: [prose('b-1', '## One\n\n### Deep\n\n## Two\n')] },
    })])?.blocks ?? null);
    expect(outline.map((item) => item.label)).toEqual(['One', 'Two']);
  });

  it('is empty for a v1 report, which has no block ids to anchor to', () => {
    expect(deriveReportOutline(null)).toEqual([]);
  });
});

describe('backlinks', () => {
  const backlink = (overrides: Partial<TrackBacklink> = {}): TrackBacklink => ({
    src_track_id: 'w-2', src_track_title: 'Cash flow model', src_block_id: 'b-9',
    dst_block_id: 'b-1', label: 'valuation', quote: null, updated_at: 0, ...overrides,
  });

  it('groups by source track and names a self-reference as such', () => {
    const groups = groupBacklinks(
      [backlink(), backlink({ src_block_id: 'b-10' }), backlink({ src_track_id: 'w-1' })],
      'w-1',
    );
    expect(groups.map((group) => [group.trackId, group.title, group.entries.length])).toEqual([
      ['w-2', 'Cash flow model', 2],
      ['w-1', 'This track (self-reference)', 1],
    ]);
  });

  it('counts backlinks per target block and ignores whole-track citations', () => {
    const counts = backlinkCountsByBlock([
      backlink(), backlink({ src_block_id: 'b-11' }), backlink({ dst_block_id: null }),
    ]);
    expect([...counts]).toEqual([['b-1', 2]]);
  });
});

describe('parseReportLink', () => {
  it('resolves a track link with a block fragment', () => {
    expect(parseReportLink('neige://wave/w-2#b-1')).toEqual({ trackId: 'w-2', blockId: 'b-1' });
  });

  it('resolves a track link without a fragment', () => {
    expect(parseReportLink('neige://wave/w-2')).toEqual({ trackId: 'w-2', blockId: null });
  });

  it('keeps a malformed percent escape as a usable raw track target', () => {
    expect(() => parseReportLink('neige://wave/%E0%A4%A')).not.toThrow();
    expect(parseReportLink('neige://wave/%E0%A4%A'))
      .toEqual({ trackId: '%E0%A4%A', blockId: null });
  });

  it('drops a malformed fragment but keeps the track', () => {
    expect(parseReportLink('neige://wave/w-2#../../etc/passwd'))
      .toEqual({ trackId: 'w-2', blockId: null });
  });

  it.each([
    ['a plain http url', 'https://example.com'],
    ['another neige noun', 'neige://area/c-1'],
    ['a javascript url', 'javascript:alert(1)'],
  ])('reads null for %s, which the renderer then shows as plain text', (_label, url) => {
    expect(parseReportLink(url)).toBeNull();
  });
});
