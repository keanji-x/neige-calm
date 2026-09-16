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
    cards={[]} mobilePanelObscured={false} canResumeTrack={false} onRenameTrack={vi.fn()} onResumeTrack={vi.fn()} onDeleteTrack={vi.fn()} onOpenTask={onOpenTask} />);
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
