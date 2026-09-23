import '../../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { TrackPage, type TrackPageProps } from './public.tsx';
import { track } from './test-fixtures.tsx';

afterEach(cleanup);

it('returns keyboard focus to the toggle when a late native declaration hides the focused panel', async () => {
  await page.viewport(1440, 900);
  const props: TrackPageProps = { track: track(), cards: [], tasks: [], openableCards: new Set(),
    mobilePanelObscured: false, canResumeTrack: false, onRenameTrack: vi.fn(), onResumeTrack: vi.fn(),
    onDeleteTrack: vi.fn(), conversationList: <button type="button">Planner entry</button> };
  const { rerender } = render(<TrackPage {...props} />);
  await page.getByRole('button', { name: 'Planner entry' }).click();
  await expect.element(page.getByRole('button', { name: 'Planner entry' })).toHaveFocus();
  rerender(<TrackPage {...props} reportPresentation="dashboard" />);
  await expect.element(page.getByRole('button', { name: 'Show track panel' })).toHaveFocus();
});
