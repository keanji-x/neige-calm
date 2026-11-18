// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { ReportTaskBlock } from './public.tsx';

afterEach(cleanup);

/** The `label / value` grid: `['Done when', '…', 'Declared by', '…']`. */
function fields(): string[] {
  return [...document.querySelectorAll('dt, dd')].map((node) => node.textContent ?? '');
}

function openTask(): void {
  const details = document.querySelector<HTMLDetailsElement>('details')!;
  details.open = true;
  fireEvent(details, new Event('toggle'));
}

describe('ReportTaskBlock', () => {
  it('says that it is a task, and names it', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 'ingest-resolver', kind: 'codex', declared_by: 'spec', ready: false,
      goal: 'Route the ingest call sites through the resolver.',
    }} />);
    expect(screen.getByText('Task')).toBeTruthy();
    expect(screen.getByText('ingest-resolver')).toBeTruthy();
    expect(screen.getByText('Codex')).toBeTruthy();
    expect(screen.getByText('Declaration not ready')).toBeTruthy();
  });

  it('puts the facts about the task in one label column', () => {
    render(<ReportTaskBlock blockId="b-1" payload={{
      key: 't', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Ship it.',
      acceptance: 'No direct call sites left.',
      gate: { steps: [{ name: 'fmt', cmd: 'cargo fmt --check' }] },
    }} />);
    expect(fields()).toEqual([]);
    openTask();
    expect(fields()).toEqual([
      'Mechanism', 'codex',
      'Provider', 'Codex',
      'Requested model / effort', 'Provider default / Provider default',
      'Effective model', '未记录 · 未记录',
      'Effective effort', '未记录 · 未记录',
      'Done when', 'No direct call sites left.',
      'Checks', 'cargo fmt --check',
      'Declared by', 'Planner agent',
    ]);
  });

  it.each([
    { model: 'attempt-model', effort: 'attempt-effort', expected: 'attempt-model / attempt-effort' },
    { model: null, effort: null, expected: 'Provider default / Provider default' },
  ])('uses frozen execution selection over a divergent declaration ($expected)', ({ model, effort, expected }) => {
    const task: ReportTaskRow = {
      blockId: 'b-1', key: 't', state: 'ready', declaration: null, status: 'running',
      statusDetail: null, kind: 'agent', provider: 'codex', requestedModel: 'new-model',
      requestedEffort: 'new-effort', workerCardId: 'worker', pendingReason: null,
      execution: {
        attemptId: 'attempt-1', generation: 1, status: 'running', label: 'Running',
        statusDetail: null, workerCardId: 'worker', blockingReason: null, createdAtMs: 1, finishedAtMs: null,
        workerSelection: {
          mechanism: 'agent', provider: 'antigravity', requested_model: model, requested_effort: effort,
          effective_model: 'observed-model', effective_effort: 'observed-effort',
          selection_source: { model: 'provider_default_reported', effort: 'provider_default_reported' },
          capabilities: { provider: 'antigravity', operation: 'acp-worker', message_transport: 'native_session',
            cancel_strategy: 'acp_cancel', resumable: true, executor: true, validator: true },
          turn_phase: 'submitting', cleanup_pending: false,
        },
      },
    };
    const { container } = render(<ReportTaskBlock blockId="b-1" task={task} payload={{
      key: 't', kind: 'agent', provider: 'codex', declared_by: 'user', ready: true, goal: 'Ship it.',
      model: 'new-model', reasoning_effort: 'new-effort',
    }} />);
    expect(container.querySelector('summary')!.textContent).toContain('Antigravity');
    openTask();
    expect(fields()).toEqual([
      'Mechanism', 'agent',
      'Provider', 'Antigravity',
      'Requested model / effort', expected,
      'Effective model', 'observed-model · Provider default',
      'Effective effort', 'observed-effort · Provider default',
      'Declared by', 'You',
    ]);
    expect(container.textContent).not.toContain('new-model');
    expect(container.textContent).not.toContain('new-effort');
    expect(container.textContent).not.toContain('Codex');
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
    openTask();
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
    expect(summary.textContent).toContain('Codex');
    expect(summary.textContent).toContain('Declaration ready');
    expect(summary.textContent).not.toContain('Route the ingest call sites');
    expect(summary.querySelector('dl')).toBeNull();
    expect(details!.querySelector('dl')).toBeNull();
    openTask();
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

  it('keeps an explicit canonical request separate from unrecorded actual values', () => {
    render(<ReportTaskBlock blockId="b-agent" payload={{
      key: 'inspect', kind: 'agent', provider: 'antigravity', declared_by: 'user', ready: true,
      goal: 'Inspect the recorded evidence.', access: 'read_only', model: 'gemini-fixture', reasoning_effort: 'deep',
    }} />);
    expect(screen.getByText('Antigravity')).toBeTruthy();
    openTask();
    expect(fields()).toEqual([
      'Mechanism', 'agent',
      'Provider', 'Antigravity',
      'Requested model / effort', 'gemini-fixture / deep',
      'Effective model', '未记录 · 未记录',
      'Effective effort', '未记录 · 未记录',
      'Declared by', 'You',
    ]);
  });
});
