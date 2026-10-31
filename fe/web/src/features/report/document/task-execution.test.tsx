import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { TaskVerdict, TrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';
import { revealReportAnchor } from '../anchor/public.ts';

afterEach(cleanup);

function report(): TrackReport {
  return { summary: '', body: '', blocks: [{
    id: 'b-task', kind: 'task', payload: {
      key: 'analyze', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Analyze the input.',
    },
  }] };
}

function verdict(status: string): TaskVerdict {
  return { blockId: 'b-task', key: 'analyze', schedulable: true, status };
}

function openReference(container: Element): void {
  const details = container.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')!;
  details.open = true;
  fireEvent(details, new Event('toggle'));
}

describe('report task execution state', () => {
  it('defers Reference task details and execution ownership until explicitly opened', () => {
    const execution = vi.fn(() => <span>Execution evidence</span>);
    const { container } = render(<ReportDocument report={report()} empty={null} renderTaskExecution={execution} />);
    expect(screen.queryByText('Analyze the input.')).toBeNull();
    expect(container.querySelector('[data-nc-task-state]')).toBeNull();
    expect(execution).not.toHaveBeenCalled();
    const reference = container.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')!;
    reference.open = true;
    fireEvent(reference, new Event('toggle'));
    const task = container.querySelector<HTMLDetailsElement>('[data-nc-task-state]')!;
    expect(task).not.toBeNull();
    expect(screen.queryByText('Analyze the input.')).toBeNull();
    expect(execution).not.toHaveBeenCalled();
    task.open = true;
    fireEvent(task, new Event('toggle'));
    expect(screen.getByText('Analyze the input.')).toBeTruthy();
    expect(execution).toHaveBeenCalled();
    task.open = false;
    fireEvent(task, new Event('toggle'));
    expect(screen.getByText('Execution evidence')).toBeTruthy();
  });

  it('loads a cold task anchor before revealing it', async () => {
    const scroll = vi.fn();
    const original = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'scrollIntoView');
    Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: scroll });
    try {
      const { container } = render(<ReportDocument report={report()} empty={null} />);
      expect(container.querySelector('[data-nc-task-state]')).toBeNull();
      revealReportAnchor('b-task');
      await waitFor(() => expect(screen.getByText('Analyze the input.')).toBeTruthy());
      expect(container.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')?.open).toBe(true);
      expect(container.querySelector<HTMLDetailsElement>('[data-nc-task-state]')?.open).toBe(true);
      expect(scroll).toHaveBeenCalled();
    } finally {
      if (original === undefined) Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
      else Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', original);
    }
  });

  it.each(['pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'])(
    'shows the same %s execution state as the task inventory instead of Ready', (status) => {
      const { container } = render(<ReportDocument report={report()} empty={null}
        taskVerdicts={[verdict(status)]} />);
      openReference(container);
      expect(container.querySelector('[data-nc-task-state]')?.getAttribute('data-nc-task-state')).toBe(status);
      expect(container.querySelector('[data-nc-task-state] > summary')?.textContent).toContain(status);
      expect(screen.queryByText('Ready')).toBeNull();
    },
  );

  it('converges from declaration through verification to failure without remounting', () => {
    const { container, rerender } = render(<ReportDocument report={report()} empty={null} />);
    openReference(container);
    expect(screen.getByText('Declaration ready')).toBeTruthy();
    const details = container.querySelector<HTMLDetailsElement>('[data-nc-task-state]')!;
    details.open = true;
    rerender(<ReportDocument report={report()} empty={null} taskVerdicts={[verdict('verifying')]} />);
    expect(details.open).toBe(true);
    expect(details.querySelector('summary')?.textContent).toContain('verifying');
    rerender(<ReportDocument report={report()} empty={null}
      taskVerdicts={[{ ...verdict('failed'), statusDetail: 'gate-red' }]} />);
    expect(details.open).toBe(true);
    expect(screen.getByText('failed').getAttribute('title')).toBe('failed — gate-red');
    expect(screen.queryByText('Declaration ready')).toBeNull();
  });

  it('keeps dependency causes available on hover', () => {
    const { container } = render(<ReportDocument report={report()} empty={null} taskVerdicts={[{
      ...verdict('pending'), pendingReason: {
        kind: 'dependencyBlocked', message: 'Waiting for `prepare`', dependencies: ['prepare'],
      },
    }]} />);
    openReference(container);
    expect(screen.getByText('pending').getAttribute('title')).toContain('Waiting for `prepare`');
  });

  it('uses the existing block identity join and ignores a verdict with a conflicting key', () => {
    const { container } = render(<ReportDocument report={report()} empty={null}
      taskVerdicts={[{ ...verdict('failed'), key: 'old-task' }]} />);
    openReference(container);
    expect(screen.queryByText('failed')).toBeNull();
    expect(screen.getByText('Declaration ready')).toBeTruthy();
  });
});
