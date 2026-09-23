import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { TrackPage, type TrackPageProps } from './public.tsx';
import { track } from './test-fixtures.tsx';

afterEach(cleanup);

function props(): TrackPageProps {
  return { track: track(), cards: [], tasks: [], openableCards: new Set(), mobilePanelObscured: false,
    canResumeTrack: false, onRenameTrack: vi.fn(), onResumeTrack: vi.fn(), onDeleteTrack: vi.fn(),
    report: <input aria-label="Report state" defaultValue="initial" />,
    conversationList: <input aria-label="Panel state" defaultValue="initial" /> };
}

it('follows a late declared presentation without remounting report or panel state', () => {
  const initial = props();
  const { rerender } = render(<TrackPage {...initial} />);
  const report = screen.getByRole('textbox', { name: 'Report state' });
  const panel = screen.getByRole('textbox', { name: 'Panel state' });
  fireEvent.change(report, { target: { value: 'selected dataset' } });
  fireEvent.change(panel, { target: { value: 'panel selection' } });
  expect(screen.queryByRole('button', { name: 'Show track panel' })).toBeNull();
  rerender(<TrackPage {...initial} reportPresentation="dashboard" />);
  expect(panel.closest('[inert]')).not.toBeNull();
  expect(screen.getByRole('button', { name: 'Show track panel' }).getAttribute('aria-expanded')).toBe('false');
  fireEvent.click(screen.getByRole('button', { name: 'Show track panel' }));
  expect(screen.getByRole('textbox', { name: 'Report state' })).toBe(report);
  expect(screen.getByRole('textbox', { name: 'Panel state' })).toBe(panel);
  expect((report as HTMLInputElement).value).toBe('selected dataset');
  expect((panel as HTMLInputElement).value).toBe('panel selection');
  rerender(<TrackPage {...initial} reportPresentation="dashboard" sideDrawerOpen />);
  expect(screen.queryByRole('button', { name: 'Hide track panel' })).toBeNull();
  rerender(<TrackPage {...initial} reportPresentation="dashboard" />);
  expect(screen.getByRole('button', { name: 'Hide track panel' }).getAttribute('aria-expanded')).toBe('true');
  fireEvent.click(screen.getByRole('button', { name: 'Hide track panel' }));
  rerender(<TrackPage {...initial} reportPresentation="dashboard" />);
  expect(screen.getByRole('button', { name: 'Show track panel' }).getAttribute('aria-expanded')).toBe('false');
  rerender(<TrackPage {...initial} track={track({ id: 'other' })} />);
  expect(panel.closest('[inert]')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Show track panel' })).toBeNull();
});

it('keeps actionable notifications outside the collapsed inventory', () => {
  const open = vi.fn();
  render(<TrackPage {...props()} reportPresentation="dashboard" onOpenInputNotification={open}
    inputNotifications={[{ origin: 'card', id: 'worker', cardId: 'worker', source: 'Worker',
      message: 'Needs a decision', state: 'awaiting-input', updatedAt: 1 }]} />);
  const review = screen.getByRole('button', { name: 'Review Worker notification: Needs a decision' });
  expect(review.closest('[inert]')).toBeNull();
  fireEvent.click(review);
  expect(open).toHaveBeenCalledWith('worker');
});
