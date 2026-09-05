import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { TaskVerdict, TrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

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

describe('report task execution state', () => {
  it.each(['pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'])(
    'shows the same %s execution state as the task inventory instead of Ready', (status) => {
      const { container } = render(<ReportDocument report={report()} empty={null}
        taskVerdicts={[verdict(status)]} />);
      expect(container.querySelector('[data-nc-task-state]')?.getAttribute('data-nc-task-state')).toBe(status);
      expect(container.querySelector('[data-nc-task-state] > summary')?.textContent).toContain(status);
      expect(screen.queryByText('Ready')).toBeNull();
    },
  );

  it('converges from declaration through verification to failure without remounting', () => {
    const { container, rerender } = render(<ReportDocument report={report()} empty={null} />);
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
    render(<ReportDocument report={report()} empty={null} taskVerdicts={[{
      ...verdict('pending'), pendingReason: {
        kind: 'dependencyBlocked', message: 'Waiting for `prepare`', dependencies: ['prepare'],
      },
    }]} />);
    expect(screen.getByText('pending').getAttribute('title')).toContain('Waiting for `prepare`');
  });

  it('uses the existing block identity join and ignores a verdict with a conflicting key', () => {
    render(<ReportDocument report={report()} empty={null}
      taskVerdicts={[{ ...verdict('failed'), key: 'old-task' }]} />);
    expect(screen.queryByText('failed')).toBeNull();
    expect(screen.getByText('Declaration ready')).toBeTruthy();
  });
});
