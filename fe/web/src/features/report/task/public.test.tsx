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
    render(<ReportTaskBlock payload={{
      key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: false,
      goal: 'Route the ingest call sites through the resolver.',
    }} />);
    expect(screen.getByText('Task')).toBeTruthy();
    expect(screen.getByText('ingest-resolver')).toBeTruthy();
    expect(screen.getByText('codex')).toBeTruthy();
    expect(screen.getByText('Not ready')).toBeTruthy();
  });

  it('puts the facts about the task in one label column', () => {
    render(<ReportTaskBlock payload={{
      key: 't', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Ship it.',
      acceptance: 'No direct call sites left.',
      gate: { steps: [{ name: 'fmt', cmd: 'cargo fmt --check' }] },
    }} />);
    expect(fields()).toEqual([
      'Done when', 'No direct call sites left.',
      'Checks', 'cargo fmt --check',
      'Declared by', 'Spec agent',
    ]);
  });

  // §8.3 — the report is an account of what happened, not the console you drive
  // it from. The legacy card had Release / Delete / Restore; this has none.
  it('offers no controls: a report does not write back', () => {
    const { container } = render(<ReportTaskBlock payload={{
      key: 't', kind: 'codex', declared_by: 'user', ready: true, goal: 'g',
    }} />);
    expect(container.querySelectorAll('button, input, [role="button"]').length).toBe(0);
  });

  // Other reports may cite the block id of a task that has since been
  // withdrawn. Dropping the row would make the document lie about its own past.
  it('keeps a withdrawn task, with both attributions', () => {
    render(<ReportTaskBlock payload={{
      key: 'walk-fallback', declared_by: 'spec', tombstoned_by: 'user',
      tombstone: { reason: 'A fallback that never runs is one nobody notices has broken.' },
    }} />);
    expect(screen.getByText('walk-fallback')).toBeTruthy();
    expect(screen.getByText('Withdrawn')).toBeTruthy();
    expect(fields()).toEqual(['Declared by', 'Spec agent', 'Withdrawn by', 'You']);
  });

  // A live task may carry an explicit `tombstone: null`, so the tombstone key is
  // not the discriminant — the attribution is.
  it('reads a task carrying an explicit null tombstone as live', () => {
    render(<ReportTaskBlock payload={{
      key: 'live', kind: 'terminal', declared_by: 'spec', ready: true, goal: 'g', tombstone: null,
    }} />);
    expect(screen.queryByText('Withdrawn')).toBeNull();
    expect(screen.getByText('Ready')).toBeTruthy();
  });
});
