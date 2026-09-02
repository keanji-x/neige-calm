import { describe, expect, it } from 'vitest';

import type { ReportTaskRow } from '../domain/report.js';
import type { CardWire } from '../domain/wave.js';
import { deriveWavePageView, taskStatusPhrase } from './wave-page.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'card-1',
    wave_id: 'w1',
    kind: 'shell',
    title: 'Main pane',
    sort: 1,
    payload: null,
    deletable: true,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

function task(overrides: Partial<ReportTaskRow> = {}): ReportTaskRow {
  return {
    blockId: 'b1',
    key: 'alpha-task',
    state: 'ready',
    declaration: null,
    status: null,
    statusDetail: null,
    kind: null,
    workerCardId: null,
    ...overrides,
  };
}

function cardsModule(cards: readonly CardWire[]) {
  return deriveWavePageView({ cards, tasks: [] }).rowModules[0];
}

function tasksModule(tasks: readonly ReportTaskRow[]) {
  return deriveWavePageView({ cards: [], tasks }).rowModules[1];
}

describe('deriveWavePageView modules', () => {
  it('derives the two row modules in the panel’s order, with their empty texts', () => {
    const view = deriveWavePageView({ cards: [], tasks: [] });

    expect(view.rowModules.map((module) => module.key)).toEqual(['cards', 'tasks']);
    expect(view.rowModules.map((module) => module.title)).toEqual(['Cards', 'Tasks']);
    expect(view.rowModules.map((module) => module.empty))
      .toEqual(['No cards yet.', 'No tasks declared yet.']);
    expect(view.rowModules.every((module) => module.rows.length === 0)).toBe(true);
  });
});

describe('deriveWavePageView cards', () => {
  it('names a titled card by its title and keeps kind as a separate field', () => {
    const [row] = cardsModule([card({ title: 'Main pane', kind: 'shell' })]).rows;

    expect(row.title).toBe('Main pane');
    expect(row.kind).toBe('shell');
  });

  /* §5.1 — mutation: make `kind` unconditional. */
  it('gives an untitled card no separate kind field: its kind is already its name', () => {
    const [row] = cardsModule([card({ title: null, kind: 'harness' })]).rows;

    expect(row.title).toBe('harness');
    expect(row.kind).toBeNull();
  });

  /* §5.2 — mutation: invert the `deletable` test. */
  it('badges a kernel-owned card, and only a kernel-owned one', () => {
    const [owned, user] = cardsModule([
      card({ id: 'card-owned', deletable: false }),
      card({ id: 'card-user', deletable: true }),
    ]).rows;

    expect(owned.badges).toEqual([{ id: 'kernel-owned', text: 'kernel-owned', struck: false }]);
    expect(user.badges).toEqual([]);
  });

  it('carries a card row’s id and no status', () => {
    const [row] = cardsModule([card({ id: 'card-7' })]).rows;

    expect(row.id).toBe('card-7');
    expect(row.status).toBeNull();
  });

  it('offers delete only on a deletable card, after opening it', () => {
    const [owned, user] = cardsModule([
      card({ id: 'card-owned', deletable: false }),
      card({ id: 'card-user', deletable: true }),
    ]).rows;

    expect(owned.actions).toEqual([{ kind: 'open-card', cardId: 'card-owned' }]);
    expect(user.actions).toEqual([
      { kind: 'open-card', cardId: 'card-user' },
      { kind: 'delete-card', cardId: 'card-user' },
    ]);
  });
});

describe('deriveWavePageView tasks', () => {
  it('names a task by its key and carries the block it reveals', () => {
    const [row] = tasksModule([task({ blockId: 'block-3', key: 'gate-alpha' })]).rows;

    expect(row.id).toBe('block-3');
    expect(row.title).toBe('gate-alpha');
    expect(row.actions).toEqual([{ kind: 'reveal-block', blockId: 'block-3' }]);
  });

  it('adds the worker-card action only when a card is running the task', () => {
    const [without, with_] = tasksModule([
      task({ blockId: 'b-a', kind: 'codex', workerCardId: null }),
      task({ blockId: 'b-b', kind: 'codex', workerCardId: 'card-9' }),
    ]).rows;

    expect(without.actions).toEqual([{ kind: 'reveal-block', blockId: 'b-a' }]);
    expect(with_.actions).toEqual([
      { kind: 'reveal-block', blockId: 'b-b' },
      { kind: 'open-card', cardId: 'card-9' },
    ]);
  });

  /*
   * The desktop's outer test, which the derivation must copy even though the
   * upstream never produces this row: `report.ts` ties `kind === null` to
   * withdrawn/unreadable/tombstoned and gives those states a null
   * `workerCardId`, so this input is production-unreachable. It is asserted
   * anyway because the rule being copied is the page's two-level condition
   * (`public.tsx:741-751`), not the upstream's invariant — a derivation that
   * only tested `workerCardId` would be right by coincidence and would hand
   * S1b's painters an action the page never draws.
   */
  it('offers no worker card when the task declares no kind, whatever the card id says', () => {
    const [row] = tasksModule([task({ blockId: 'b-k', kind: null, workerCardId: 'card-9' })]).rows;

    expect(row.actions).toEqual([{ kind: 'reveal-block', blockId: 'b-k' }]);
  });

  it('strikes the declaration badge of a withdrawn task and no other', () => {
    const [withdrawn, unreadable] = tasksModule([
      task({ blockId: 'b-w', state: 'withdrawn', declaration: 'Withdrawn' }),
      task({ blockId: 'b-u', state: 'unreadable', declaration: 'Unreadable' }),
    ]).rows;

    expect(withdrawn.badges).toEqual([{ id: 'declaration', text: 'Withdrawn', struck: true }]);
    expect(unreadable.badges).toEqual([{ id: 'declaration', text: 'Unreadable', struck: false }]);
  });

  /*
   * §5.3 — the declaration/status precedence.
   *
   * `deriveReportTasks` already guarantees a row never carries both, so a
   * mutation that *re-applies* that rule here (drop the declaration badge when
   * a status exists) is invisible against production-shaped input: it agrees
   * with the upstream on every row the upstream can produce. The property that
   * is actually load-bearing at this layer is therefore the negative one — this
   * derivation holds **no** precedence of its own — and pinning it needs a row
   * the upstream would not emit. That is the point: the day the join changes
   * its mind, the panel must follow it rather than out-vote it in a second
   * place.
   */
  it('reads declaration and status independently, imposing no precedence of its own', () => {
    const [row] = tasksModule([task({
      declaration: 'Not ready',
      status: 'running',
      statusDetail: null,
    })]).rows;

    expect(row.badges).toEqual([{ id: 'declaration', text: 'Not ready', struck: false }]);
    expect(row.status).toEqual({ token: 'running', phrase: 'running' });
  });

  it('carries the task’s worker kind, and null when the declaration names none', () => {
    const [typed, untyped] = tasksModule([
      task({ blockId: 'b-t', kind: 'codex' }),
      task({ blockId: 'b-u', kind: null }),
    ]).rows;

    expect(typed.kind).toBe('codex');
    expect(untyped.kind).toBeNull();
  });

  it('has no status when the row may report no run', () => {
    const [row] = tasksModule([task({ status: null, statusDetail: 'ignored' })]).rows;

    expect(row.status).toBeNull();
  });

  /* §5.4 — mutation: drop `statusDetail`. */
  it('keeps the kernel’s reason in the phrase while the token stays the bare word', () => {
    const [row] = tasksModule([task({ status: 'failed', statusDetail: 'wave is not a git repository' })]).rows;

    expect(row.status).toEqual({
      token: 'failed',
      phrase: 'failed — wave is not a git repository',
    });
  });
});

/* §5.5 — mutation: drop the ` — detail` join. Same site as §5.4 when exercised
   through the derivation, so the wording is pinned at its own function too. */
describe('taskStatusPhrase', () => {
  it('is the bare status when the kernel gave no reason', () => {
    expect(taskStatusPhrase('running', null)).toBe('running');
  });

  it('appends the reason to the status, never substituting it', () => {
    expect(taskStatusPhrase('failed', 'boom')).toBe('failed — boom');
  });
});
