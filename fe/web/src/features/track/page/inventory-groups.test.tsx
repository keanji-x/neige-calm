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
it('keeps execution folded and discloses current and completed tasks with their original actions', () => {
  const onOpenTask = vi.fn();
  const view = render(<TrackPage track={track()} tasks={[task('finished-a', 'done'), task('active-a', 'running'), task('finished-b', 'done')]}
    cards={[]} openableCards={new Set()} mobilePanelObscured={false} canReopenTrack={false} canCloseTrack={false} adminGrantedAt={null} onRenameTrack={vi.fn()} closedControl={{ pending: false, setClosed: () => Promise.resolve(true), feedback: { error: null, clear: () => undefined, run: () => Promise.resolve(true) } }} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} onOpenTask={onOpenTask} />);
  const completed = view.container.querySelector<HTMLElement>('[data-nc-work-inventory] section[aria-label="Progress"]')!;
  expect(completed.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  expect(completed.querySelector('h2')?.textContent).toBe('Progress');
  expect(completed.querySelector('[data-nc-row]')).toBeNull();
  fireEvent.click(completed.querySelector('button[aria-expanded]')!);
  expect(completed.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('true');
  fireEvent.click(screen.getByRole('button', { name: 'finished-a' }));
  expect(onOpenTask).toHaveBeenCalledWith('finished-a');
});

it('keeps failed executions folded while retaining explanations in the task description', () => {
  const onOpenTask = vi.fn();
  const view = render(<TrackPage track={track()} tasks={[
    { ...task('verify-layout', 'failed'), statusDetail: 'Gate exited with code 1' },
    task('choose-layout', 'blocked'),
  ]} cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} adminGrantedAt={null} onRenameTrack={vi.fn()}
    closedControl={{ pending: false, setClosed: () => Promise.resolve(true), feedback: { error: null, clear: () => undefined, run: () => Promise.resolve(true) } }} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} onOpenTask={onOpenTask} />);
  const progress = view.container.querySelector<HTMLElement>('[data-nc-work-inventory] section[aria-label="Progress"]')!;
  expect(progress.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  fireEvent.click(progress.querySelector('button[aria-expanded]')!);
  expect(screen.getByRole('button', { name: 'verify-layout' }).getAttribute('aria-description')).toBe('failed — Gate exited with code 1');
  expect(screen.queryByText('Gate exited with code 1', { exact: true })).toBeNull();
  expect(screen.getByRole('button', { name: 'verify-layout' }).getAttribute('aria-description')).toContain('Gate exited with code 1');
  fireEvent.click(screen.getByRole('button', { name: 'verify-layout' }));
  expect(onOpenTask).toHaveBeenCalledWith('verify-layout');
});

it('keeps a failure without additional detail in one compact status line', () => {
  render(<TrackPage track={track()} tasks={[task('verify', 'failed')]}
    cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} adminGrantedAt={null} onRenameTrack={vi.fn()}
    closedControl={{ pending: false, setClosed: () => Promise.resolve(true), feedback: { error: null, clear: () => undefined, run: () => Promise.resolve(true) } }} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Progress' }));
  expect(screen.getAllByText('failed')).toHaveLength(1);
});

it('shows a label-only failed execution without inventing an explanation', () => {
  const view = render(<TrackPage track={track()} tasks={[{ ...task('verify', 'failed'), execution: {
    attemptId: 'attempt-1', generation: 1, createdAtMs: 1000, finishedAtMs: null, status: 'failed', label: 'Failed',
    statusDetail: null, workerCardId: null, blockingReason: null,
  } }]} cards={[]} openableCards={new Set()} mobilePanelObscured={false}
    canReopenTrack={false} canCloseTrack={false} adminGrantedAt={null} onRenameTrack={vi.fn()}
    closedControl={{ pending: false, setClosed: () => Promise.resolve(true), feedback: { error: null, clear: () => undefined, run: () => Promise.resolve(true) } }} onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Progress' }));
  const row = view.container.querySelector('[data-nc-row="task:verify"]')!;
  expect(row.querySelector('[data-nc-task-status-text]')?.getAttribute('aria-hidden')).toBe('true');
  expect(screen.getAllByText('failed')).toHaveLength(1);
});
