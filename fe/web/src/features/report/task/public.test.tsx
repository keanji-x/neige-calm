// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ReportTaskBlock } from './public.tsx';

afterEach(cleanup);

/** The `label / value` grid: `['Done when', '…', 'Declared by', '…']`. */
function fields(): string[] {
  return [...document.querySelectorAll('dt, dd')].map((node) => node.textContent ?? '');
}

describe('ReportTaskBlock', () => {
  // Every other kind announces itself by its shape; a task is structured text,
  // and without the word it reads as prose that has gone strange.
  it('says that it is a task, and names it', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: false,
      goal: 'Route the ingest call sites through the resolver.',
    }} />);
    expect(screen.getByText('Task')).toBeTruthy();
    expect(screen.getByText('ingest-resolver')).toBeTruthy();
    expect(screen.getByText('codex')).toBeTruthy();
    expect(screen.getByText('Not ready')).toBeTruthy();
  });

  it('puts the facts about the task in one label column', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 't', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Ship it.',
      acceptance: 'No direct call sites left.',
      gate: { steps: [{ name: 'fmt', cmd: 'cargo fmt --check' }] },
    }} />);
    expect(fields()).toEqual([
      'Done when', 'No direct call sites left.',
      'Checks', 'cargo fmt --check',
      'Declared by', 'Planner agent',
    ]);
  });

  // §8.3 — the report is an account of what happened, not the console you drive
  // it from. The legacy card had Release / Delete / Restore; this has none.
  it('offers no controls: a report does not write back', () => {
    const { container } = render(<ReportTaskBlock blockId="b-1" payload={{
      key: 't', kind: 'codex', declared_by: 'user', ready: true, goal: 'g',
    }} />);
    expect(container.querySelectorAll('button, input, [role="button"]').length).toBe(0);
  });

  // Other reports may cite the block id of a task that has since been
  // withdrawn. Dropping the row would make the document lie about its own past.
  it('keeps a withdrawn task, with both attributions', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'walk-fallback', declared_by: 'spec', tombstoned_by: 'user',
      tombstone: { reason: 'A fallback that never runs is one nobody notices has broken.' },
    }} />);
    expect(screen.getByText('walk-fallback')).toBeTruthy();
    expect(screen.getByText('Withdrawn')).toBeTruthy();
    expect(fields()).toEqual(['Declared by', 'Planner agent', 'Withdrawn by', 'You']);
  });

  /*
   * ── The fold ─────────────────────────────────────────────────────────────
   *
   * Three separate claims, because they can break independently: that the block
   * *is* a `<details>` (so the platform gives us Enter, Space, find-in-page and
   * print-expanded for free), that the head is its `<summary>` (so what stays
   * when folded is which task and whether it is ready — not a bare word `Task`
   * with no key beside it), and that it starts **closed**.
   *
   * The default reversed once, and the reason is recorded in `public.tsx`:
   * these blocks no longer sit in the prose, they sit inside the document's one
   * collapsed `Reference` appendix. Left open, opening that appendix would dump
   * every worker prompt and gate command the change exists to get out of the
   * reading column. The row is the reference; the block is what it points at.
   *
   * The head's own contents are asserted here rather than left to the test
   * above: moving a field out of `<summary>` is exactly the regression that
   * would leave a folded task unidentifiable, and it is invisible to any test
   * that only asks whether the text is *somewhere* in the document.
   */
  it('folds: the head is the summary, the detail is the body, and it opens closed', () => {
    const { container } = render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: true,
      goal: 'Route the ingest call sites through the resolver.',
      acceptance: 'No direct call sites left.',
    }} />);

    const details = container.querySelector('details');
    expect(details).not.toBeNull();
    expect(details!.open).toBe(false);

    const summary = details!.querySelector('summary')!;
    /* Folded, this row is the whole block, so it has to carry the identity. */
    expect(summary.textContent).toContain('Task');
    expect(summary.textContent).toContain('ingest-resolver');
    expect(summary.textContent).toContain('codex');
    expect(summary.textContent).toContain('Ready');
    /* And the detail is *outside* it, or nothing is folded away. */
    expect(summary.textContent).not.toContain('Route the ingest call sites');
    expect(summary.querySelector('dl')).toBeNull();
    expect(details!.querySelector('dl')).not.toBeNull();
  });

  /* A withdrawn task folds too. It is the smaller of the two shapes and the one
     a reader is least likely to want open, so shipping the fold on only the live
     branch would be the wrong half. */
  it('folds a withdrawn task as well, on the same summary', () => {
    const { container } = render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'walk-fallback', declared_by: 'spec', tombstoned_by: 'user',
      tombstone: { reason: 'A fallback that never runs is one nobody notices has broken.' },
    }} />);
    const summary = container.querySelector('details > summary');
    expect(summary).not.toBeNull();
    expect(summary!.textContent).toContain('walk-fallback');
    expect(summary!.textContent).toContain('Withdrawn');
    expect(summary!.textContent).not.toContain('A fallback that never runs');
  });

  /* `key` is `z.string()` on the wire, so an empty one is legal. The block id
     stands in — the same fallback `deriveReportTasks` applies for the panel,
     because otherwise the panel names this row `b-1` and the block it points at
     names it nothing. */
  it('falls back to the block id when the task declared an empty key', () => {
    render(<ReportTaskBlock blockId="b_bf88" payload={{
      key: '', kind: 'codex', declared_by: 'spec', ready: true, goal: 'g',
    }} />);
    expect(screen.getByText('b_bf88')).toBeTruthy();
  });

  // A live task may carry an explicit `tombstone: null`, so the tombstone key is
  // not the discriminant — the attribution is.
  it('reads a task carrying an explicit null tombstone as live', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'live', kind: 'terminal', declared_by: 'spec', ready: true, command: 'true', tombstone: null,
    }} />);
    expect(screen.queryByText('Withdrawn')).toBeNull();
    expect(screen.getByText('Ready')).toBeTruthy();
  });
});
