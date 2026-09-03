import { describe, expect, it } from 'vitest';

import type { CardWire } from './wave.js';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, groupBacklinks, hasLiveTaskRun,
  parseReportLink, readWaveReport, TASK_STATUS_DETAIL_LIMIT, WAVE_REPORT_CARD_KIND, waveTaskVerdictsOperation,
  type TaskVerdict, type WaveBacklink,
} from './report.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'c1', wave_id: 'w1', kind: WAVE_REPORT_CARD_KIND, title: null, sort: 0,
    payload: {}, deletable: false, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

function prose(id: string, markdown: string) {
  return { id, kind: 'prose', rev: 1, payload: { markdown } };
}

describe('readWaveReport', () => {
  it('reads summary and body out of the wave-report card', () => {
    const cards = [
      card({ id: 'other', kind: 'codex', payload: { body: 'not this one' } }),
      card({ payload: { schemaVersion: 3, docRev: 7, summary: 'One line', body: '# Goal\n\nDo the thing.' } }),
    ];
    expect(readWaveReport(cards)).toEqual({ summary: 'One line', body: '# Goal\n\nDo the thing.', blocks: null });
  });

  it('keeps a report whose summary is written while its body is blank', () => {
    expect(readWaveReport([card({ payload: { summary: 'Agent finished the migration.', body: '  ' } })]))
      .toEqual({ summary: 'Agent finished the migration.', body: '', blocks: null });
  });

  // A payload from a newer schema must stay readable: this surface renders a
  // known set of block kinds and has no business rejecting a document because
  // the persistence layer grew a field.
  it('ignores fields it does not render, including ones it has never seen', () => {
    const cards = [card({ payload: { schemaVersion: 9, docRev: 1, summary: 's', body: 'b', future: {} } })];
    expect(readWaveReport(cards)?.body).toBe('b');
  });

  it.each([
    ['no report card at all', [card({ kind: 'codex' })]],
    ['a payload that is not an object', [card({ payload: 'nope' })]],
    ['the untouched payload a fresh wave carries', [card({ payload: {} })]],
    ['a body that is only whitespace', [card({ payload: { body: '   \n  ' } })]],
    ['a blank body next to an empty blocks array', [card({ payload: { body: '', blocks: [] } })]],
  ])('reads null for %s', (_label, cards) => {
    expect(readWaveReport(cards as readonly CardWire[])).toBeNull();
  });

  // The null cases above are one state to a reader — "nothing written yet" —
  // and a wave that was created and never worked on is the common one. It must
  // not be distinguishable from a parse failure at the call site, because the
  // call site would then have to render an error for it.
  it('does not distinguish a fresh wave from an unreadable payload', () => {
    expect(readWaveReport([card({ payload: {} })]))
      .toEqual(readWaveReport([card({ payload: 42 })]));
  });

  it('reads the typed blocks, which are what carries each block id', () => {
    const report = readWaveReport([card({
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

  it('drops one wire-invalid block while keeping the other blocks', () => {
    const report = readWaveReport([card({
      payload: {
        body: '# Goal',
        blocks: [prose('b-1', '# Goal'), { id: '', kind: 'prose', payload: {} }, prose('b-2', 'Still here.')],
      },
    })]);
    expect(report?.blocks?.map((block) => block.id)).toEqual(['b-1', 'b-2']);
  });

  it('falls back to body when blocks is not an array', () => {
    expect(readWaveReport([card({ payload: { body: '# Goal', blocks: 'nope' } })]))
      .toEqual({ summary: '', body: '# Goal', blocks: null });
  });

  it('keeps a task live when an absent optional field is explicit null', () => {
    const report = readWaveReport([card({
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
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('app');
  });

  // The entrance fee of the block model: a report is written by an agent, so it
  // will contain something this build does not know about. That must cost one
  // block, never the page.
  it.each([
    ['a kind this build has never seen', { id: 'b-1', kind: 'chart.sankey', rev: 1, payload: { nodes: [] } }],
    ['a known kind whose payload does not fit', { id: 'b-1', kind: 'table', rev: 1, payload: { columns: [] } }],
    ['a known kind whose payload is not an object', { id: 'b-1', kind: 'prose', rev: 1, payload: 7 }],
  ])('degrades %s to one unsupported block, keeping the others', (_label, bad) => {
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [bad, prose('b-2', 'Still here.')] },
    })]);
    expect(report?.blocks?.[0]).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: bad.kind });
    expect(report?.blocks?.[1]?.kind).toBe('prose');
  });

  // `src` is loaded into an iframe, so its validator is the one place a
  // rejected payload is a security outcome and not a rendering one.
  it.each([
    ['a protocol-relative path', '//evil.example/x'],
    ['a backslash the browser would normalize to a slash', '/\\evil.example/x'],
    ['an absolute URL', 'https://evil.example/x'],
    ['a control character', '/apps/a\0b'],
  ])('refuses an app block whose src is %s', (_label, src) => {
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('unsupported');
  });
});

describe('deriveReportTasks', () => {
  function tasksOf(blocks: unknown[], verdicts?: readonly TaskVerdict[]) {
    return deriveReportTasks(
      readWaveReport([card({ payload: { body: 'x', blocks } })])?.blocks ?? null,
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

  /*
   * CHANGED SHAPE — there were two helpers here, `declaration(text)` and
   * `runtime(text)`, because a row carried one `note` and the helpers said
   * which half of the join had written it. A row now carries `declaration`,
   * `status` and `kind` as three fields, so the rows below are written out
   * whole: the assertions that matter are which of the three are populated
   * together, and a helper that assembled them would be the production
   * function's own precedence rules copied into the test.
   */

  it('lists every task in document order and nothing else', () => {
    expect(tasksOf([
      prose('b-1', '# One\n'),
      task('b-2', live('alpha', true)),
      { id: 'b-3', kind: 'table', rev: 1, payload: { caption: 'c', columns: [], rows: [] } },
      task('b-4', live('beta', false)),
    ])).toEqual([
      { blockId: 'b-2', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null },
      { blockId: 'b-4', key: 'beta', state: 'not-ready', declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null },
    ]);
  });

  /* Same discriminant as the block renderer, and the same trap: a *live* task
     may carry an explicit `tombstone: null`, so the presence of that key proves
     nothing. Reading it as withdrawn would strike through a task that is
     running. */
  it('reads a task carrying an explicit null tombstone as live, not withdrawn', () => {
    expect(tasksOf([task('b-1', { ...live('alpha', true), tombstone: null })]))
      .toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null }]);
  });

  /* Kept for the same reason the block keeps it: the task existed, other
     reports may cite its block id, and a panel that dropped it would disagree
     with the document it is derived from. */
  it('keeps a withdrawn task', () => {
    expect(tasksOf([task('b-1', {
      key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: { reason: 'r' },
    })])).toEqual([{
      blockId: 'b-1', key: 'gone', state: 'withdrawn',
      declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null,
    }]);
  });

  /*
   * **A task whose payload does not parse still gets a row**, and this reversed
   * once. The first cut dropped it, on the reasoning that `unsupported` has no
   * key and no state so a row would name a task the document does not draw.
   * Both review channels found the same hole in that: the document *does* draw
   * it — `features/report/document` lifts it into the `Reference` appendix —
   * and the outline is allowed to skip tasks precisely *because* the panel
   * lists them. Dropped here, that one block was in no index at all, reachable
   * only by scrolling. Its id stands in for the key: not a substitute, but the
   * literal other reports cite it by, which is the one thing still true.
   */
  it('keeps a task block whose payload does not parse, named by its id', () => {
    expect(tasksOf([task('b-1', { key: 'broken' })])).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null,
    }]);
  });

  /*
   * The two rows that must never carry a `kind`, asserted for what the panel
   * does with it rather than for the field: the kind is the *only* control that
   * opens a worker card (#1149), so `kind: null` is how a withdrawn or
   * unreadable row is guaranteed to offer no card affordance at all. It is a
   * second lock on the same rule `workerCardId: null` already states — and the
   * one that survives even if a future verdict ever reached these rows, because
   * a tombstone payload has no `kind` field to read and an unreadable payload
   * has nothing readable in it.
   */
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

  /* `key` is `z.string()` on the wire, so an empty one is legal and reaches the
     panel as a button with no text — no accessible name, nothing to click, and
     invisible to `getByRole`. The block id stands in, exactly as it does for an
     unreadable task. */
  it('falls back to the block id when the task declared an empty key', () => {
    expect(tasksOf([task('b-1', live('', true))]))
      .toEqual([{ blockId: 'b-1', key: 'b-1', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null }]);
  });

  /* And only a block that declared itself a task: an `unsupported` block of
     some other kind is a figure this build cannot draw, which belongs in the
     argument, not in the panel's list of work. */
  it('does not claim an unsupported block that declared some other kind', () => {
    expect(tasksOf([{ id: 'b-1', kind: 'chart.sankey', rev: 1, payload: {} }])).toEqual([]);
  });

  it('has no rows for a report with no blocks', () => {
    expect(deriveReportTasks(null)).toEqual([]);
  });

  /* ── The runtime join ───────────────────────────────────────────────── */

  /* The panel's whole complaint: a user who dispatched four tasks could not
     tell which was running or which worker card had it. Both facts now come off
     one verdict, and the kind comes off the declaration — the verdict does not
     carry one. */
  it('reads a dispatched task as its status and its kind, and carries the worker card', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready',
      declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9',
    }]);
  });

  /*
   * CHANGED EXPECTATION — these three cases used to assert one formatted string
   * each (`verifying · claude`, `done · terminal`, `canceled · codex`). The
   * separator was the join's typography, and the panel now draws the two halves
   * as different things: the status is a dot with a label, the kind is the
   * control that opens the worker card. So the assertion is that both facts
   * arrive, unedited and apart — a row that concatenated them again would fail
   * here, which is the point.
   */
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

  /*
   * CHANGED EXPECTATION — `failed` used to be printed *alone*, dropping the
   * worker kind, so that a reader scanning for the broken row did not have to
   * read past a worker name. That was a rule about one shared word slot. The
   * kind is now the row's card control and `failed` is a red dot at the row's
   * trailing edge, so suppressing the kind would take the click-through away
   * from the one row a reader most wants to open. Both are carried.
   */
  it('carries failed and the kind together, and keeps the worker card', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready',
      declaration: null, status: 'failed', statusDetail: null, kind: 'codex', workerCardId: 'card-9',
    }]);
  });

  /* ── The kernel's reason (#1147 `status_detail` / #1149 acceptance) ─────
     `failed` is a word the reader can already see. Why it failed is the thing
     they were about to go looking for, and the kernel now says it. */

  it('carries the kernel\'s reason for a status alongside the status word', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({
        blockId: 'b-1', key: 'alpha', status: 'failed',
        statusDetail: 'wave 9a4c is not a git repository', workerCardId: 'card-9',
      })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: 'failed',
      statusDetail: 'wave 9a4c is not a git repository', kind: 'codex', workerCardId: 'card-9',
    }]);
  });

  /* The reason qualifies the status word and has nowhere to attach without
     one: a verdict for a task with no `tasks` row can still carry prose (the
     projection writes a diagnostic before anything is dispatched), and printing
     it would decorate a row this build has just decided reports no run. */
  it('drops the reason when the verdict carries no status to qualify', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: null, statusDetail: 'never dispatched' })],
    )[0]).toMatchObject({ status: null, statusDetail: null, declaration: null });
  });

  /* Withdrawn and unreadable rows take no runtime state *at all*, and the
     reason is runtime state. A withdrawn task's `tasks` row outlives the
     withdrawal, so its verdict keeps carrying both — and the struck row must
     keep saying `Withdrawn` and nothing else. */
  it.each([
    ['withdrawn', { key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }],
    ['unreadable', { key: 'broken' }],
  ])('gives a %s row no reason either, not just no status', (_label, payload) => {
    const row = tasksOf(
      [task('b-1', payload)],
      [verdict({
        blockId: 'b-1', key: 'gone', status: 'failed',
        statusDetail: 'wave 9a4c is not a git repository', workerCardId: 'card-9',
      })],
    )[0];
    expect(row?.status).toBeNull();
    expect(row?.statusDetail).toBeNull();
  });

  /* Same rule from the other end: a key two live blocks claim has no owner,
     so the kernel sends `status: null` for both (#1160) — and a reason with no
     status to qualify is dropped here rather than printed bare. */
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

  /* A blank reason is not a reason. `''` would render as a dangling separator
     — `failed — ` — which says the kernel spoke when it did not. */
  it.each([['empty', ''], ['whitespace only', '   \n  ']])('reads a %s reason as none', (_label, detail) => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', statusDetail: detail })],
    )[0]?.statusDetail).toBeNull();
  });

  /* The reason's only destination is an accessible name and a `title`, neither
     of which a stylesheet can truncate: a screen reader reads the whole
     `aria-label` and a tooltip is not styleable at all. A kernel message with a
     newline in it would also print the newline literally in the tooltip. So the
     row carries one bounded line, decided here rather than at the renderer. */
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

  /*
   * The bound counts UTF-16 code units and the cut must not land inside a
   * character. `😀` is a surrogate pair, and placed so it straddles the cut it
   * used to be sliced in half: the row carried a lone high surrogate, which is
   * `�` in the tooltip and a replacement character in the accessible name — a
   * kernel reason ending in a glyph the kernel never wrote.
   *
   * Asserted on the code points rather than on a rendered string, because a
   * lone surrogate compares equal to nothing useful and `toBe` on a literal
   * would be a test nobody can read. The bound itself still has to hold.
   */
  it('never cuts a reason in the middle of an astral character', () => {
    const detail = tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({
        blockId: 'b-1', key: 'alpha', status: 'failed',
        statusDetail: `${'x'.repeat(TASK_STATUS_DETAIL_LIMIT - 2)}😀…`,
      })],
    )[0]?.statusDetail ?? '';
    /* Premise: this reason is over the limit, so the cut really happens. */
    expect(`${'x'.repeat(TASK_STATUS_DETAIL_LIMIT - 2)}😀…`.length)
      .toBeGreaterThan(TASK_STATUS_DETAIL_LIMIT);
    expect([...detail].some((point) => {
      const code = point.codePointAt(0) ?? 0;
      return code >= 0xd800 && code <= 0xdfff;
    })).toBe(false);
    expect(detail.length).toBeLessThanOrEqual(TASK_STATUS_DETAIL_LIMIT);
    expect(detail.endsWith('…')).toBe(true);
  });

  /* And the boundary itself is not truncated: a reason exactly at the limit is
     whole, so the ellipsis only ever means "there was more". */
  it('leaves a reason exactly at the limit untouched', () => {
    const whole = 'y'.repeat(TASK_STATUS_DETAIL_LIMIT);
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'failed', statusDetail: whole })],
    )[0]?.statusDetail).toBe(whole);
  });

  /*
   * `schedulable` does NOT mean "not waiting on a dependency", and an earlier
   * cut of this join read it that way: `pending && !schedulable` printed
   * `blocked`. The kernel clears the flag for every candidate past the spec
   * ceiling or the wave-tree budget (`task_projection.rs`, the
   * `verdicts[index].schedulable = false` after the `spec_task_ceiling`
   * diagnostic), so an ordinary queue behind capacity rendered as `blocked` and
   * a healthy wave looked stuck. Task-budget reasoning is out of this slice, so
   * both rows get the one word the kernel actually gave them.
   */
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
  });

  /* An unassigned non-pending status is printed as it stands rather than being
     forced into one of the four words: the kernel owns this vocabulary and a
     status this build has not heard of is more useful shown than hidden. */
  it('prints an unassigned status that is neither pending nor failed as it stands', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'done' })],
    )[0]?.status).toBe('done');
  });

  /*
   * A verdict with no `status` is a *declaration* verdict: the projection ran,
   * but the `tasks` table has no row for this key, which is exactly "declared
   * and never dispatched". `schedulable` is still true here, and reading it as
   * a run would invent a state the kernel never reported.
   */
  it('keeps the declaration word for a verdict that carries no status at all', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', schedulable: false })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'not-ready',
      declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null,
    }]);
  });

  /*
   * The other side of that: once there IS a status, the readiness word stands
   * down. This is the one precedence rule the single `note` field encoded that
   * is about meaning rather than about layout — `Not ready` describes a
   * declaration the kernel has since dispatched anyway, and a row printing both
   * would be arguing with itself. Carrying the two fields separately makes it
   * possible to state both at once, which is why it is pinned here rather than
   * left to fall out of a formatter.
   */
  it('drops the readiness word once the kernel reports a run', () => {
    const row = tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )[0];
    expect(row?.declaration).toBeNull();
    expect(row?.state).toBe('not-ready');
    expect(row?.status).toBe('running');
  });

  /* And a ready task with no verdict at all stays silent — the first render,
     before the verdict read lands, must look like the list this panel shipped
     with rather than a hole. */
  it('renders the declaration-only list when no verdicts have arrived', () => {
    const row = tasksOf([task('b-1', live('alpha', true))])[0];
    expect(row?.status).toBeNull();
    expect(row?.declaration).toBeNull();
    /* The kind is a declaration fact and does not wait for a verdict: it is
       what the row shows before anything has run. */
    expect(row?.kind).toBe('codex');
  });

  /* A verdict naming a task no block declares is dropped: it would be a row
     with nothing in the document behind it, which is the one thing this list
     promised never to be. */
  it('drops a verdict whose key and block id match nothing in the report', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-99', key: 'ghost', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null }]);
  });

  /* The other direction: a declared task with no verdict keeps its declaration
     word while its neighbour reports a run. */
  it('leaves a task with no verdict alone while its neighbour runs', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true)), task('b-2', live('beta', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    ).map((row) => [row.status, row.kind])).toEqual([['running', 'codex'], [null, 'codex']]);
  });

  /* `blockId` is the identity join and wins. The fallback matters when the
     cached report card and the projection disagree about block ids — the two
     are written by different paths and a stale card is the ordinary state
     between an edit and its refetch. */
  it('joins by block id first and falls back to the key', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-other', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )[0]?.status).toBe('running');
  });

  /*
   * The trap in that fallback: an unreadable task's `key` is its *block id*, so
   * matching that literal against the key index would report one task's run on
   * another task's row. Only a key the block actually declared may be looked
   * up. (An unreadable row takes no runtime word at all now — see below — so
   * this is belt and braces, and it is the belt that is load-bearing if the
   * unreadable rule is ever relaxed.)
   */
  it('never matches an unreadable task through the block id standing in for its key', () => {
    expect(tasksOf(
      [task('b-1', { key: 'broken' })],
      [verdict({ blockId: 'b-other', key: 'b-1', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null,
    }]);
  });

  /* Not even through its own block id. This build could not read the block's
     key, so it cannot vouch that a verdict naming that key is about this task;
     `Unreadable` is the one thing it can still say truthfully. */
  it('keeps Unreadable even when a verdict names the very block, and offers no card', () => {
    expect(tasksOf(
      [task('b-1', { key: 'broken' })],
      [verdict({ blockId: 'b-1', key: 'broken', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'b-1', state: 'unreadable',
      declaration: 'Unreadable', status: null, statusDetail: null, kind: null, workerCardId: null,
    }]);
  });

  /*
   * A withdrawn declaration keeps saying it was withdrawn.
   *
   * CHANGED EXPECTATION — this test used to assert the opposite ("reports a run
   * on a withdrawn task rather than its declaration word"), on the theory that
   * a task withdrawn mid-flight still has a live worker worth reporting. What
   * that produced in the panel was worse than the state it described:
   * withdrawing an already-dispatched task leaves its `tasks` row alive, so the
   * verdict keeps carrying `status` — commonly `done` — and the row rendered an
   * un-struck `done` with the word `Withdrawn` gone entirely, while the click
   * flipped to the worker card so the withdrawn block could no longer be
   * revealed from the panel at all. The panel's job here is to say the task
   * existed and was withdrawn; the worker card is reachable from CARDS.
   *
   * It also settles the duplicate below: a key that is withdrawn and then
   * redeclared has two blocks, and the kernel stamps the live run onto the
   * verdicts of both.
   */
  it('keeps the declaration word and no card for a withdrawn task that had already run', () => {
    expect(tasksOf(
      [task('b-1', { key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} })],
      [verdict({ blockId: 'b-1', key: 'gone', status: 'done', workerCardId: 'card-9', schedulable: false })],
    )).toEqual([{
      blockId: 'b-1', key: 'gone', state: 'withdrawn',
      declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null,
    }]);
  });

  /*
   * Two rows, one run — the duplication the kernel can genuinely produce.
   *
   * `attach_task_read_state` indexes the `tasks` table by key alone and stamps
   * `status` / `workerCardId` onto EVERY verdict carrying that key, and the
   * projection emits a verdict for a tombstoned declaration as well as for the
   * live one. Withdraw `alpha`, redeclare `alpha` as a new block, and
   * `taskDiagnostics` contains two verdicts both reporting the live run. Only
   * the live block may print it.
   */
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
      { blockId: 'b-old', key: 'alpha', state: 'withdrawn', declaration: 'Withdrawn', status: null, statusDetail: null, kind: null, workerCardId: null },
      { blockId: 'b-new', key: 'alpha', state: 'ready', declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9' },
    ]);
  });

  /*
   * The other duplicate, and the one the redeclare rule above does NOT settle:
   * two blocks declaring the same key while BOTH are live.
   *
   * Reachable, not hypothetical. `tasks` is `UNIQUE (wave_id, key)` (migration
   * 0058) so only one run can exist, and `plan.upsert` rejects duplicates only
   * *within one batch* (`plan.rs` `validate_new_batch`); the projection accepts
   * the document and merely flags each block with a `duplicate_key` diagnostic
   * (`calm-types/src/report_blocks/tasks.rs`) and carries on.
   *
   * There is no owner to pick: the kernel itself calls this document invalid.
   * **So the kernel says so on the wire.** `attach_task_read_state` attaches
   * run state only to the single live declaration of a key (#1160), so a
   * contested key arrives here as `status: null` / `workerCardId: null` on
   * *every* block that names it — this build is asserting the shape the server
   * produces, not re-deriving the rule from the document.
   *
   * The rows still exist and keep their declaration word; what they lack is a
   * run either of them could be shown to own.
   */
  it('reports no run on either row when two live blocks claim the same key', () => {
    expect(tasksOf(
      [task('b-one', live('alpha', true)), task('b-two', live('alpha', false))],
      [
        verdict({ blockId: 'b-one', key: 'alpha', status: null, workerCardId: null }),
        verdict({ blockId: 'b-two', key: 'alpha', status: null, workerCardId: null }),
      ],
    )).toEqual([
      { blockId: 'b-one', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null },
      {
        blockId: 'b-two', key: 'alpha', state: 'not-ready',
        declaration: 'Not ready', status: null, statusDetail: null, kind: 'codex', workerCardId: null,
      },
    ]);
  });

  /*
   * **The skew this build accepts, stated so it cannot rot into a surprise.**
   *
   * The two halves of the join are two cached queries — the blocks ride the
   * wave detail (`['wave', waveId]`), the verdicts have their own key
   * (`['wave-report', waveId]`) — so one can land a refresh the other missed.
   * The reviewable worry: a lone block's run is cached, a second live block
   * appears on the same key, the *blocks* refresh but the verdict refetch
   * fails, and the row keeps a run the kernel would now abstain from. React
   * Query keeps the last good data across a failed refetch, and a terminal
   * status is outside `eventlessWindowTaskStatuses`, so nothing here restarts
   * the poll on its own.
   *
   * **It is a skew, not a misattribution, and that is the whole reason the
   * removed `contestedLiveKeys` pre-pass is not worth restoring.** The join is
   * by block id, so a row can only ever display the run the kernel attached to
   * *that block*, at the moment it said so. It cannot acquire a neighbour's
   * run: the second block finds no verdict of its own, and `rowBlockIds` keeps
   * the first block's verdict out of the key index precisely so the fallback
   * cannot reach it. The stale row is a fact that was true of itself one
   * snapshot ago — the same staleness every cached read in this app carries —
   * and it converges on the next `wave.report_edited` or `task.*` event, on
   * window focus, and on remount. Re-deriving contention from the blocks would
   * buy a few seconds of that convergence back at the price of a second
   * authority on ownership, which is the thing #1160 removed.
   *
   * That argument holds *only while the verdict's own block is still a row* —
   * see the next case, where it is not, and where `rowBlockIds` therefore
   * protects nothing. This one also pins the other end of the fix that case
   * required: `alpha` is contested here too, and `b-one` still shows its run,
   * because the identity join is not what was narrowed.
   */
  it('keeps a run on the block the kernel gave it to when the verdicts lag the blocks', () => {
    expect(tasksOf(
      // The document already has two live blocks on `alpha` …
      [task('b-one', live('alpha', true)), task('b-two', live('alpha', true))],
      // … but the cached verdicts are from before `b-two` existed.
      [verdict({ blockId: 'b-one', key: 'alpha', status: 'done', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-one', 'done', 'card-1'],
      // Never decorated by its neighbour's verdict, which is the failure that
      // would actually mislead.
      ['b-two', null, null],
    ]);
  });

  /*
   * **The skew that IS a misattribution, and the half of the gate that stops
   * it.** #1160 review round two overturned the round-one verdict above, which
   * argued `rowBlockIds` already covered this: it does, but only for as long as
   * the block the stale verdict names is still in the document.
   *
   * The sequence, all inside one edit: `alpha` is declared by block A, runs to
   * a terminal status, its verdict is cached — then A is **hard-deleted**
   * (`wave_report_doc.rs` `delete_block` leaves no block-level tombstone) and
   * two new blocks are pasted, both declaring `alpha`. The blocks query
   * refreshes; the `['wave-report', waveId]` query does not, and React Query
   * keeps the stale verdict. `b-A` now names no row, so the verdict-side gate
   * lets it into the key index, and *both* new rows miss at their own ids and
   * fall back onto it: one dead task's `done` and its worker card, painted on
   * two live rows at once, with a terminal status that no poll will revisit.
   *
   * So the row side gates too: a key more than one row claims is not a usable
   * fallback for any of them. This is not `contestedLiveKeys` returning — it
   * decides nothing about *ownership* and overrides no verdict; it counts how
   * many rows would consult one index entry and declines to guess between them.
   */
  it('gives no row a run through the key when two rows claim that key', () => {
    expect(tasksOf(
      // A (`b-gone`) was hard-deleted; two fresh blocks took its key.
      [task('b-two', live('alpha', true)), task('b-three', live('alpha', false))],
      // The cached verdict is A's, and A is in no row now.
      [verdict({ blockId: 'b-gone', key: 'alpha', status: 'done', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId, row.declaration])).toEqual([
      ['b-two', null, null, null],
      ['b-three', null, null, 'Not ready'],
    ]);
  });

  /* The rule is about how many rows *claim* the key, so a tombstone counts:
     withdraw `alpha`, redeclare it, and an id-less verdict on `alpha` is as
     unattributable as it is between two live blocks — the projection emits one
     verdict per declaration, so the key alone never picked a row out. (The
     withdrawn row takes no decoration for its own reasons; the live one is the
     assertion.) */
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

  /*
   * **The price of counting tombstones, pinned so nobody "fixes" it.**
   *
   * This is the same rule as above at its most expensive: `alpha` has exactly
   * ONE live declaration (`b-new`), and the cached verdict is the kernel's
   * synthesised one for the deleted declaration — `blockId: ''`, so the key is
   * the only way it could arrive. Because the tombstone `b-old` still counts as
   * a row claiming `alpha`, the fallback is suppressed and `b-new` renders
   * blank even though it is the unambiguous live owner.
   *
   * **Blank is the expected value here, not a bug to be repaired by filtering
   * tombstones out of `keysDeclaredByMoreThanOneRow`.** Two reasons, both in
   * that function's comment and in `verdictFor`'s:
   *
   * 1. An id-less verdict says exactly *"when the kernel looked, no live
   *    declaration owned this key"*. A live block present in the document now
   *    is not evidence that the old run was **its** run; handing it over is a
   *    guess, and #1160's position throughout is that an unattributable run is
   *    reported to nobody.
   * 2. Maintainability, which is the stronger reason. The counting rule is
   *    deliberately *syntactic* — `isTaskBlock` + `declaredTaskKey`, nothing
   *    about liveness or ownership — so it cannot disagree with the kernel.
   *    Adding a tombstone filter re-imports the live/tombstone decision into
   *    this module, which is the shape of the `contestedLiveKeys` pre-pass this
   *    PR deleted, and that decision is fiddly here (`tombstoned_by` is the
   *    discriminant; a live block may carry `tombstone: null`). A second copy
   *    would not fail loudly when the kernel changes representation.
   *
   * So: if you make this test green by skipping tombstones, you have not fixed
   * a defect, you have taken the trade the other way. Read the two comments
   * first.
   */
  it('leaves the sole live claimant blank by design when a tombstone shares its key', () => {
    expect(tasksOf(
      [
        task('b-old', { key: 'alpha', declared_by: 'spec', tombstoned_by: 'user', tombstone: {} }),
        task('b-new', live('alpha', true)),
      ],
      // The kernel's verdict for the declaration it no longer sees: id-less,
      // reachable only by key, and still reporting a run.
      [verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-old', null, null],
      // Blank on purpose. Not `['b-new', 'running', 'card-1']`.
      ['b-new', null, null],
    ]);
  });

  /* And the narrowing is scoped: one row on the key still takes the fallback,
     which is the case the fallback exists for — the kernel's synthesised
     verdict for a deleted declaration carries `blockId: ''` and has nothing but
     the key to arrive by. A rule that refused every fallback would take that
     with it. */
  it('still falls back to the key when exactly one row claims it', () => {
    expect(tasksOf(
      [task('b-new', live('alpha', true)), task('b-other', live('beta', true))],
      [verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-1' })],
    ).map((row) => [row.blockId, row.status, row.workerCardId])).toEqual([
      ['b-new', 'running', 'card-1'],
      ['b-other', null, null],
    ]);
  });

  /* And it is scoped to the contested key: the honest row beside it still
     reports its own run. The kernel scopes the refusal per key, and this build
     must not widen it — a rule that quieted the whole panel because one key was
     declared twice would cost more than the defect it fixes. */
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

  /*
   * The empty key is one key like any other, on both sides of the wire.
   * `tasks` is `UNIQUE (wave_id, key)` with `key` a plain string, so two blocks
   * declaring `''` still share exactly one row — and the kernel's uniqueness
   * rule groups `''` with everything else rather than skipping it (#1160,
   * `live_declaration_blocks_by_key`), so both verdicts arrive undecorated.
   *
   * The rows still exist and still carry their fallback names — the display
   * name falls back to the block id, which is what made these two look like two
   * distinct tasks when they were both handed the same run.
   */
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

  /* And one live block with no declared key is not contested with itself: the
     single-empty-key row keeps its run, which is what stops the rule above from
     being a blanket refusal to report unnamed tasks. */
  it('still reports the run of a lone block that declared no key', () => {
    expect(tasksOf(
      [task('b-one', live('', true))],
      [verdict({ blockId: 'b-one', key: '', status: 'running', workerCardId: 'card-9' })],
    ).map((row) => [row.key, row.status, row.workerCardId])).toEqual([
      ['b-one', 'running', 'card-9'],
    ]);
  });

  /*
   * `''` is not a status either, and it is the one empty string that would have
   * got through: it is not `null`, so it silences the declaration word, opens
   * the `statusDetail` gate, and renders as `data-nc-status=""` — a dot
   * matching no form, and an accessible name reading `Status:  — boom`, a
   * kernel reason with no state attached. Today's `TaskStatus` serialises to a
   * fixed lowercase word, so this hardens the wire type (`z.string().nullish()`)
   * rather than a writer, on the same line of reasoning as the empty
   * `workerCardId` and the empty `key` above.
   */
  it('treats an empty status as no run at all, reason and declaration word included', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', false))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: '', statusDetail: 'boom', workerCardId: 'card-9' })],
    )).toEqual([{
      blockId: 'b-1', key: 'alpha', state: 'not-ready', declaration: 'Not ready',
      status: null, statusDetail: null, kind: 'codex', workerCardId: 'card-9',
    }]);
  });

  /*
   * The key fallback is for a verdict that names no block this report has —
   * never a licence to accept a block-id hit that contradicts the key. Both
   * identifiers are present on both sides here and they disagree, which means
   * the cached card and the projection are describing different documents; the
   * one thing that cannot be right is reporting `beta`'s run on `alpha`'s row.
   */
  /*
   * The same rule from the other side, and the one a single-row report cannot
   * catch: the verdict names a block that EXISTS, just not this one.
   *
   * `b-alpha` finds nothing at its own id, so it reaches the key index — and
   * before the fix that index held `b-beta`'s verdict, because it carries the
   * key `alpha`. The row then printed `b-beta`'s run and, worse, offered
   * `b-beta`'s worker card as `b-alpha`'s. A verdict that named a block said
   * which row it is about; it may decorate that row or none.
   *
   * `b-beta`'s own row keeps its block-id hit and refuses it on the key
   * contradiction, which is the rule above — so the correct answer is that this
   * verdict decorates nothing at all.
   */
  it('never lets a verdict naming another report block reach a row through the key index', () => {
    expect(tasksOf(
      [task('b-alpha', live('alpha', true)), task('b-beta', live('beta', true))],
      [verdict({ blockId: 'b-beta', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([
      { blockId: 'b-alpha', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null },
      { blockId: 'b-beta', key: 'beta', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null },
    ]);
  });

  /* The fallback the rule above must not have cost: a verdict whose block id
     names NO row still reaches its row by key. This is the ordinary state
     between a report edit and the projection catching up, and it is the only
     way a run is reported at all when the two block-id spaces have drifted. */
  it('still reaches a row by key when the verdict names a block this report does not have', () => {
    expect(tasksOf(
      [task('b-alpha', live('alpha', true)), task('b-beta', live('beta', true))],
      [verdict({ blockId: 'b-stale', key: 'alpha', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([
      { blockId: 'b-alpha', key: 'alpha', state: 'ready', declaration: null, status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'card-9' },
      { blockId: 'b-beta', key: 'beta', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null },
    ]);
  });

  it('ignores a block-id hit whose key contradicts the block declaration', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'beta', status: 'running', workerCardId: 'card-9' })],
    )).toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null, status: null, statusDetail: null, kind: 'codex', workerCardId: null }]);
  });

  /*
   * **A contradicting block-id hit ends the lookup on purpose — it does not
   * fall through to the key index.** The test above cannot tell the two apart,
   * because its key index is empty either way; this one puts a perfectly good
   * `alpha` entry there and still demands a blank row.
   *
   * The setup needs a block id to be reused, which is a real property, not a
   * hypothetical: `report_blocks/align.rs` mints ids by FNV-1a with linear
   * probing and re-inherits them heuristically, so a hard-deleted block's id
   * can be handed to a different declaration. Here the stale verdict for the
   * *old* `b-1` (key `beta`) is still cached while the new `b-1` declares
   * `alpha`; the kernel's id-less `alpha` verdict is also in hand and would
   * have reached this row by key had the id lookup simply missed.
   *
   * **The blank is expected, and it is the fail-closed direction.** A hit that
   * names this block while naming another key is not evidence about this row —
   * it says the cached card and the projection are describing different
   * documents — and "the id index contradicted itself" is not a reason to go
   * trust the key index instead. Making this row show `running` again means
   * deciding that a self-contradicting hit may be discarded silently, which is
   * the guess this module refuses everywhere else.
   */
  it('stops at a contradicting block-id hit by design and never retries through the key index', () => {
    expect(tasksOf(
      [task('b-1', live('alpha', true))],
      [
        // The reused id: this verdict is about the block `b-1` used to be.
        verdict({ blockId: 'b-1', key: 'beta', status: 'done', workerCardId: 'card-8' }),
        // Reachable only by key, and it IS in the key index — exactly one row
        // claims `alpha`, so nothing else suppresses it.
        verdict({ blockId: '', key: 'alpha', status: 'running', workerCardId: 'card-9' }),
      ],
    )).toEqual([{
      // Neither `done`/`card-8` (the contradicting hit) nor `running`/`card-9`
      // (the fallback this must not reach).
      blockId: 'b-1', key: 'alpha', state: 'ready', declaration: null,
      status: null, statusDetail: null, kind: 'codex', workerCardId: null,
    }]);
  });

  /* `''` is not a card id. The wire types `workerCardId` as an optional string,
     and an empty one would route the panel's click at a card that cannot
     exist — the row must fall back to revealing its block. */
  it('treats an empty worker card id as no card at all', () => {
    const row = tasksOf(
      [task('b-1', live('alpha', true))],
      [verdict({ blockId: 'b-1', key: 'alpha', status: 'pending', workerCardId: '' })],
    )[0];
    expect(row?.workerCardId).toBeNull();
    expect(row?.status).toBe('pending');
  });
});

/*
 * The predicate that decides whether the panel's refresh timer runs at all.
 *
 * It is NOT the kernel's `TaskStatus::is_terminal` read from the other side,
 * and an earlier cut that made it one is the defect these tests pin. The timer
 * exists for one write — `scheduler::mark_running`, which flips
 * `dispatched → running` and stamps `worker_card_id` while emitting nothing —
 * so the only statuses that may keep it alive are the ones inside that
 * eventless window. Every other status, terminal or not, is bracketed by events
 * that already invalidate this query, and polling it would be an unbounded cost
 * for no convergence: nothing bounds how long a row may sit in one status.
 *
 * The failure modes are asymmetric, which is why the unknown case is pinned
 * here — a word this build has not heard of must NOT keep the timer alive, or
 * the day the kernel adds a terminal status every settled wave in every open
 * tab polls forever.
 */
describe('hasLiveTaskRun', () => {
  /*
   * **Rows, and produced by the real join.** The predicate used to read the raw
   * verdicts, and reading them is what made its own comment false: the kernel
   * emits a verdict for a *deleted* declaration, which names no block here, so
   * an in-flight status can exist with no row anywhere on screen for the poll
   * to converge to. Hand-written rows would let
   * that class of defect back in silently — a fixture can make any row it
   * likes — so every row below comes out of `deriveReportTasks` against real
   * declarations, which is the function the panel calls.
   */
  const blocksOf = (blocks: unknown[]) =>
    readWaveReport([card({ payload: { body: 'x', blocks } })])?.blocks ?? null;
  const declared = (id: string, key: string) =>
    ({ id, kind: 'task', rev: 1, payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' } });
  const rowsFor = (verdicts: TaskVerdict[], blocks: unknown[] = [declared('b-1', 'k')]) =>
    deriveReportTasks(blocksOf(blocks), verdicts);
  const at = (status: string | null) => rowsFor([{
    blockId: 'b-1', key: 'k', schedulable: true, status, workerCardId: null,
  }]);

  /* Premise: the join really produced a decorated row, or every assertion below
     is true for the wrong reason. */
  it('decorates the row it is asked about', () => {
    expect(at('running').map((row) => row.status)).toEqual(['running']);
  });

  /* `dispatched` is where the eventless window opens (`task.dispatched` fired
     with `worker_card_id` still NULL) and `running` is what the silent write
     produces. Nothing else will say so. */
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

  /*
   * **`pending` does not poll**, and this is the whole point of the narrowing.
   *
   * A pending row's only exit is the claim tx, which emits
   * `Event::TaskDispatched` — and `task.dispatched` is in
   * `taskVerdictInvalidatingKinds`, so the panel is told. The timer would buy
   * nothing and cost without limit: a task behind a zero task budget, behind a
   * dependency that failed, or left pending by a canceled wave stays pending
   * for the life of the wave, and every open tab on that wave would refetch the
   * whole document projection every 3 seconds for as long as it stayed focused.
   */
  it('is false for a wave holding nothing but pending rows, so a stuck wave does not poll', () => {
    expect(hasLiveTaskRun(at('pending'))).toBe(false);
    expect(hasLiveTaskRun(rowsFor([
      { blockId: 'b-1', key: 'a', schedulable: false, status: 'pending', workerCardId: null },
      { blockId: 'b-2', key: 'b', schedulable: true, status: 'pending', workerCardId: null },
      { blockId: 'b-3', key: 'c', schedulable: false, status: 'canceled', workerCardId: null },
    ], [declared('b-1', 'a'), declared('b-2', 'b'), declared('b-3', 'c')]))).toBe(false);
  });

  /*
   * `verifying` does not poll either, for the same reason from both sides. It
   * is only ever entered by `task_report_success_from_worker_tx`, whose two
   * call sites emit `Event::TaskCompleted` in the same tx, and only left
   * through `reconcile_gate_outcome`'s `task.gate_result` / `task.completed` /
   * `task.failed` — all four invalidate this query. Its `worker_card_id` was
   * stamped long before and arrived with that entry event, so there is nothing
   * for a poll to find, and a parked gate can sit for hours.
   */
  it('is false while a gate verifies, which is evented on both sides', () => {
    expect(hasLiveTaskRun(at('verifying'))).toBe(false);
  });

  /* A declared task with no `tasks` row has not run. The write that creates
     that row emits `task.dispatched`, which invalidates this query — so there
     is nothing here for a timer to converge. */
  it('is false when a task has no status at all', () => {
    expect(hasLiveTaskRun(at(null))).toBe(false);
    expect(hasLiveTaskRun([])).toBe(false);
    expect(hasLiveTaskRun(undefined)).toBe(false);
  });

  it('is false for a status this build does not know, so an unknown word cannot poll forever', () => {
    expect(hasLiveTaskRun(at('quiescent'))).toBe(false);
  });

  /* One live row among settled ones is the ordinary mid-wave state. */
  it('is true when any one row is live', () => {
    expect(hasLiveTaskRun(rowsFor([
      { blockId: 'b-1', key: 'a', schedulable: true, status: 'done', workerCardId: 'c1' },
      { blockId: 'b-2', key: 'b', schedulable: true, status: 'running', workerCardId: 'c2' },
    ], [declared('b-1', 'a'), declared('b-2', 'b')]))).toBe(true);
  });

  /*
   * ── A run with no row on screen is not something a timer can converge ────
   *
   * The kernel synthesises a verdict for a declaration that has been *deleted*
   * from the document: `blockId: ''`, a key no block here declares. It names no
   * row, `deriveReportTasks` builds none for it, and nothing in the panel will
   * ever change when it settles — so a 3 s refetch on its account is a cost
   * with no observable, which is exactly what the predicate's own comment says
   * it does not do.
   */
  it('is false for a live verdict whose declaration was deleted from the report', () => {
    const verdicts: TaskVerdict[] = [{
      blockId: '', key: 'deleted-task', schedulable: true, status: 'running', workerCardId: 'c-9',
    }];
    const rows = rowsFor(verdicts);
    expect(rows.map((row) => [row.key, row.status])).toEqual([['k', null]]);
    expect(hasLiveTaskRun(rows)).toBe(false);
  });

  /*
   * And the other rowless in-flight run: a key two live blocks both claim. The
   * kernel refuses to name an owner and sends `status: null` for both (#1160),
   * so the panel shows two undecorated rows however long the run takes — and
   * there is nothing here a 3 s refetch could converge.
   */
  it('is false for a live run on a key two live declarations both claim', () => {
    const rows = rowsFor([
      { blockId: 'b-1', key: 'dup', schedulable: true, status: null, workerCardId: null },
      { blockId: 'b-2', key: 'dup', schedulable: true, status: null, workerCardId: null },
    ], [declared('b-1', 'dup'), declared('b-2', 'dup')]);
    expect(rows.map((row) => row.status)).toEqual([null, null]);
    expect(hasLiveTaskRun(rows)).toBe(false);
  });
});

describe('waveTaskVerdictsOperation', () => {
  it('GETs the wave report route with the id escaped', () => {
    const operation = waveTaskVerdictsOperation('w/1');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/waves/w%2F1/report');
  });

  /* Only `taskDiagnostics` is read. The rest of the response repeats the report
     card this page already holds, and decoding it here would give the document
     two sources that can disagree. */
  it('reads only the task diagnostics out of the response', () => {
    expect(waveTaskVerdictsOperation('w1').responseSchema.parse({
      schemaVersion: 3, docRev: 9, summary: 's', body: 'b', blocks: [{ id: 'b-1' }],
      taskDiagnostics: [{
        blockId: 'b-1', key: 'alpha', schedulable: true, status: 'running',
        workerCardId: 'card-9', gateResult: null, diagnostics: [],
      }],
    })).toEqual([{
      blockId: 'b-1', key: 'alpha', schedulable: true, status: 'running', workerCardId: 'card-9',
    }]);
  });

  /* `statusDetail` is the kernel's own reason for the status (#1147). It is
     read off the same verdict rather than fetched separately, which is the only
     way the two can never disagree — and it must survive the decode, or the row
     model above has nothing to carry. */
  it('reads the kernel\'s status detail off the verdict', () => {
    expect(waveTaskVerdictsOperation('w1').responseSchema.parse({
      taskDiagnostics: [{
        blockId: 'b-1', key: 'alpha', schedulable: true, status: 'failed',
        statusDetail: 'wave 9a4c is not a git repository', diagnostics: [],
      }],
    })).toEqual([{
      blockId: 'b-1', key: 'alpha', schedulable: true, status: 'failed',
      statusDetail: 'wave 9a4c is not a git repository',
    }]);
  });

  /* Fail-soft, exactly as one malformed block costs only that block: one
     unreadable verdict costs that row's runtime word and nothing else. */
  it('drops a malformed verdict and keeps the rest', () => {
    expect(waveTaskVerdictsOperation('w1').responseSchema.parse({
      taskDiagnostics: [
        { blockId: 'b-1', key: 'alpha', schedulable: 'yes' },
        { blockId: 'b-2', key: 'beta', schedulable: false },
      ],
    })).toEqual([{ blockId: 'b-2', key: 'beta', schedulable: false }]);
  });

  /* A response with no diagnostics field at all is "no verdicts", not a decode
     failure — the panel must degrade to the declaration-only list. */
  it('reads an absent taskDiagnostics as no verdicts', () => {
    expect(waveTaskVerdictsOperation('w1').responseSchema.parse({ summary: 's' })).toEqual([]);
  });
});

describe('deriveReportOutline', () => {
  it('numbers sections continuously across prose blocks, never restarting per block', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [prose('b-1', '# One\n\ntext\n\n## Two\n'), prose('b-2', '# Three\n')],
      },
    })])?.blocks ?? null);
    expect(outline.map((item) => [item.number, item.label, item.blockId])).toEqual([
      [1, 'One', 'b-1-h1'],
      [2, 'Two', 'b-1-h2'],
      [3, 'Three', 'b-2-h1'],
    ]);
  });

  it('hangs a non-prose block under the section above it, as evidence rather than a section', () => {
    const outline = deriveReportOutline(readWaveReport([card({
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

  /* Tasks are no longer drawn in the flow — `features/report/document` lifts
     them into the collapsed `Reference` appendix — so hanging them under the
     section they used to follow would point the outline at a place they are not.
     On a real wave that was eight rows of machinery in a map of the argument.

     The assertion pairs a task with a *kept* non-prose block, because "the
     outline is empty" would also pass a rule that dropped everything. */
  it('leaves task blocks out: they are not in the document flow any more', () => {
    const outline = deriveReportOutline(readWaveReport([card({
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

  /* Including one whose payload did not parse. Keying the skip on
     `kind === 'task'` alone listed exactly those — an outline row labelled
     `task`, pointing at a block the document had moved into the appendix. */
  it('leaves out a task whose payload did not parse, which degrades to unsupported', () => {
    const outline = deriveReportOutline(readWaveReport([card({
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
    const outline = deriveReportOutline(readWaveReport([card({
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

  // Deeper than H2 is not a section: `REPORT_MAX_DEPTH` is the kernel's own
  // splitting rule, and an outline that listed H3s would promise a level the
  // document does not have.
  it('ignores headings deeper than the report max depth', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: { body: 'x', blocks: [prose('b-1', '# One\n\n### Deep\n')] },
    })])?.blocks ?? null);
    expect(outline.map((item) => item.label)).toEqual(['One']);
  });

  it('is empty for a v1 report, which has no block ids to anchor to', () => {
    expect(deriveReportOutline(null)).toEqual([]);
  });
});

describe('backlinks', () => {
  const backlink = (overrides: Partial<WaveBacklink> = {}): WaveBacklink => ({
    src_wave_id: 'w-2', src_wave_title: 'Cash flow model', src_block_id: 'b-9',
    dst_block_id: 'b-1', label: 'valuation', quote: null, updated_at: 0, ...overrides,
  });

  it('groups by source wave and names a self-reference as such', () => {
    const groups = groupBacklinks(
      [backlink(), backlink({ src_block_id: 'b-10' }), backlink({ src_wave_id: 'w-1' })],
      'w-1',
    );
    expect(groups.map((group) => [group.waveId, group.title, group.entries.length])).toEqual([
      ['w-2', 'Cash flow model', 2],
      ['w-1', 'This wave (self-reference)', 1],
    ]);
  });

  it('counts backlinks per target block and ignores whole-wave citations', () => {
    const counts = backlinkCountsByBlock([
      backlink(), backlink({ src_block_id: 'b-11' }), backlink({ dst_block_id: null }),
    ]);
    expect([...counts]).toEqual([['b-1', 2]]);
  });
});

describe('parseReportLink', () => {
  it('resolves a wave link with a block fragment', () => {
    expect(parseReportLink('neige://wave/w-2#b-1')).toEqual({ waveId: 'w-2', blockId: 'b-1' });
  });

  it('resolves a wave link without a fragment', () => {
    expect(parseReportLink('neige://wave/w-2')).toEqual({ waveId: 'w-2', blockId: null });
  });

  it('keeps a malformed percent escape as a usable raw wave target', () => {
    expect(() => parseReportLink('neige://wave/%E0%A4%A')).not.toThrow();
    expect(parseReportLink('neige://wave/%E0%A4%A'))
      .toEqual({ waveId: '%E0%A4%A', blockId: null });
  });

  // Landing at the top of the right report beats a dead link, so a malformed
  // fragment costs the fragment and not the destination.
  it('drops a malformed fragment but keeps the wave', () => {
    expect(parseReportLink('neige://wave/w-2#../../etc/passwd'))
      .toEqual({ waveId: 'w-2', blockId: null });
  });

  it.each([
    ['a plain http url', 'https://example.com'],
    ['another neige noun', 'neige://area/c-1'],
    ['a javascript url', 'javascript:alert(1)'],
  ])('reads null for %s, which the renderer then shows as plain text', (_label, url) => {
    expect(parseReportLink(url)).toBeNull();
  });
});
