import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { TrackPage } from './public.tsx';
import { track } from './test-fixtures.tsx';
import type { ReportTaskRow } from '../../../../../core/domain/report.ts';

afterEach(cleanup);
function task(blockId: string, status: string): ReportTaskRow {
  return { blockId, key: blockId, status, statusDetail: null, kind: null, workerCardId: null,
    state: 'ready', declaration: null, pendingReason: null };
}
it('shows working tasks and discloses completed tasks with their original actions', () => {
  const onOpenTask = vi.fn();
  const view = render(<TrackPage track={track()} tasks={[task('finished-a', 'done'), task('active-a', 'running'), task('finished-b', 'done')]}
    cards={[]} openableCards={new Set()} mobilePanelObscured={false} canReopenTrack={false} canCloseTrack={false} onRenameTrack={vi.fn()} onReopenTrack={vi.fn()} onCloseTrack={vi.fn()} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} onOpenTask={onOpenTask} />);
  const working = view.container.querySelector<HTMLDetailsElement>('[data-nc-inventory-group="working"]')!;
  const completed = view.container.querySelector<HTMLDetailsElement>('[data-nc-inventory-group="done"]')!;
  expect(working.open).toBe(true);
  expect(completed.open).toBe(false);
  expect(completed.querySelector('summary')?.getAttribute('aria-label')).toBe('Completed, 2 tasks');
  fireEvent.click(completed.querySelector('summary')!);
  expect(completed.open).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: 'finished-a' }));
  expect(onOpenTask).toHaveBeenCalledWith('finished-a');
});

it('exposes problem groups and keeps explanations in the task description', () => {
  const onOpenTask = vi.fn();
  const view = render(<TrackPage track={track()} tasks={[
    { ...task('verify-layout', 'failed'), statusDetail: 'Gate exited with code 1' },
    task('choose-layout', 'blocked'),
  ]} cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} onRenameTrack={vi.fn()}
    onReopenTrack={vi.fn()} onCloseTrack={vi.fn()} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} onOpenTask={onOpenTask} />);
  expect(view.container.querySelector<HTMLDetailsElement>('[data-nc-inventory-group="failed"]')?.open).toBe(true);
  expect(view.container.querySelector<HTMLDetailsElement>('[data-nc-inventory-group="attention"]')?.open).toBe(true);
  expect(screen.queryByText('Gate exited with code 1', { exact: true })).toBeNull();
  expect(screen.getByRole('button', { name: 'verify-layout' }).getAttribute('aria-description')).toContain('Gate exited with code 1');
  fireEvent.click(screen.getByRole('button', { name: 'verify-layout' }));
  expect(onOpenTask).toHaveBeenCalledWith('verify-layout');
});

it('keeps a failure without additional detail in one compact status line', () => {
  render(<TrackPage track={track()} tasks={[task('verify', 'failed')]}
    cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} onRenameTrack={vi.fn()}
    onReopenTrack={vi.fn()} onCloseTrack={vi.fn()} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} />);
  expect(screen.getAllByText('failed')).toHaveLength(1);
});

it('shows a label-only failed execution without inventing an explanation', () => {
  const view = render(<TrackPage track={track()} tasks={[{ ...task('verify', 'failed'), execution: {
    attemptId: 'attempt-1', generation: 1, status: 'failed', label: 'Failed',
    statusDetail: null, workerCardId: null, blockingReason: null,
  } }]} cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} onRenameTrack={vi.fn()}
    onReopenTrack={vi.fn()} onCloseTrack={vi.fn()} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} />);
  const row = view.container.querySelector('[data-nc-row="verify"]')!;
  expect(row.querySelectorAll('[aria-hidden="true"]')).toHaveLength(1);
  expect(screen.getAllByText('failed')).toHaveLength(1);
});
