import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import type { TrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

it('preview refuses live context even when a caller supplies a resolver', () => {
  const resolver = vi.fn(() => ({ rows: [{ asset: 'PRIVATE POSITION' }] }));
  const report: TrackReport = { summary: '', body: '', blocks: [{ id: 'table', kind: 'layout', payload: {
    version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{ kind: 'table', title: 'Positions', span: 1,
      data: { source: 'neige://plugin/market/holdings' }, columns: [{ key: 'asset', label: 'Asset', format: 'text', digits: 0 }],
    }],
  } }] };
  const preview = { mode: 'preview' as const };
  render(<ReportDocument {...preview} report={report} empty={null} resolveLiveTable={resolver}/>);
  expect(resolver).not.toHaveBeenCalled();
  expect(screen.getByRole('columnheader', { name: 'Asset' })).toBeTruthy();
  expect(screen.queryByText('PRIVATE POSITION')).toBeNull();
});

it('preview never mounts an embedded app while normal reports retain it', () => {
  const report: TrackReport = { summary: '', body: '', blocks: [{ id: 'app', kind: 'app', payload: { src: '/preview-app-must-not-load', title: 'App configuration', height: 240 } }] };
  const preview = { mode: 'preview' as const };
  const { container, rerender } = render(<ReportDocument {...preview} report={report} empty={null}/>);
  expect(container.querySelector('iframe')).toBeNull();
  rerender(<ReportDocument report={report} empty={null}/>);
  expect(container.querySelector('iframe')?.getAttribute('src')).toBe('/preview-app-must-not-load');
});

it('preview shows only saved task declarations and never calls an execution renderer', () => {
  const report: TrackReport = { summary: '', body: '', blocks: [{ id: 'task', kind: 'task', payload: {
    key: 'plan', kind: 'codex', declared_by: 'user', ready: false, goal: 'Saved task definition',
  } }] };
  const execute = vi.fn(() => <button>Execute private task</button>);
  const preview = { mode: 'preview' as const };
  const { container } = render(<ReportDocument {...preview} report={report} empty={null}
    taskVerdicts={[{ blockId: 'task', key: 'plan', schedulable: true, status: 'running' }]} renderTaskExecution={execute}/>);
  expect(execute).not.toHaveBeenCalled();
  expect(container.querySelector('[data-nc-task-state]')?.getAttribute('data-nc-task-state')).toBe('not-ready');
});

it('preview never resolves or enables copied research URLs even when navigation is supplied', () => {
  const resolveAppLink = vi.fn(() => ({ trackId: 'private-study', blockId: null }));
  const onOpenLink = vi.fn();
  const report: TrackReport = { summary: '', body: '', blocks: [{ id: 'table', kind: 'layout', payload: {
    version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{ kind: 'table', title: 'Research', span: 1,
      data: { rows: [{ name: 'Saved research', track: 'https://app.example/next/track/private-study' }] },
      columns: [{ key: 'name', label: 'Research', format: 'text', digits: 0, linkKey: 'track' }],
    }],
  } }] };
  render(<ReportDocument mode="preview" report={report} empty={null} resolveAppLink={resolveAppLink} onOpenLink={onOpenLink}/>);
  expect(screen.getByRole('cell', { name: 'Saved research' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Saved research' })).toBeNull();
  expect(resolveAppLink).not.toHaveBeenCalled();
  expect(onOpenLink).not.toHaveBeenCalled();
});
