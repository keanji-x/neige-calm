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
  it('says that it is a task, and names it', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: false,
      goal: 'Route the ingest call sites through the resolver.',
    }} />);
    expect(screen.getByText('Task')).toBeTruthy();
    expect(screen.getByText('ingest-resolver')).toBeTruthy();
    expect(screen.getByText('codex')).toBeTruthy();
    expect(screen.getByText('Declaration not ready')).toBeTruthy();
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

  it('offers no controls: a report does not write back', () => {
    const { container } = render(<ReportTaskBlock blockId="b-1" payload={{
      key: 't', kind: 'codex', declared_by: 'user', ready: true, goal: 'g',
    }} />);
    expect(container.querySelectorAll('button, input, [role="button"]').length).toBe(0);
  });

  it('keeps a withdrawn task, with both attributions', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'walk-fallback', declared_by: 'spec', tombstoned_by: 'user',
      tombstone: { reason: 'A fallback that never runs is one nobody notices has broken.' },
    }} />);
    expect(screen.getByText('walk-fallback')).toBeTruthy();
    expect(screen.getByText('Withdrawn')).toBeTruthy();
    expect(fields()).toEqual(['Declared by', 'Planner agent', 'Withdrawn by', 'You']);
  });

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
    expect(summary.textContent).toContain('Task');
    expect(summary.textContent).toContain('ingest-resolver');
    expect(summary.textContent).toContain('codex');
    expect(summary.textContent).toContain('Declaration ready');
    expect(summary.textContent).not.toContain('Route the ingest call sites');
    expect(summary.querySelector('dl')).toBeNull();
    expect(details!.querySelector('dl')).not.toBeNull();
  });

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

  it('falls back to the block id when the task declared an empty key', () => {
    render(<ReportTaskBlock blockId="b_bf88" payload={{
      key: '', kind: 'codex', declared_by: 'spec', ready: true, goal: 'g',
    }} />);
    expect(screen.getByText('b_bf88')).toBeTruthy();
  });

  it('reads a task carrying an explicit null tombstone as live', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'live', kind: 'terminal', declared_by: 'spec', ready: true, command: 'true', tombstone: null,
    }} />);
    expect(screen.queryByText('Withdrawn')).toBeNull();
    expect(screen.getByText('Declaration ready')).toBeTruthy();
  });
});
