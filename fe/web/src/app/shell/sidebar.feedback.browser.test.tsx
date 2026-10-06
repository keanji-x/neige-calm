// #2175-17: a failed sidebar write (here, a pin the server refused) is said in the rail as the Astryx error banner, in
// a real engine, light and dark.
import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { Sidebar } from './sidebar.tsx';
import styles from './shell.module.css';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

it.each(['light', 'dark'] as const)('says a refused pin in the rail as an error banner (%s)', async (theme) => {
  await page.viewport(1280, 720);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = [{ id: 'next', title: 'Next task', areaId: area.id, sort: 1, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 10, ...NEUTRAL_ACTIVITY, activityAt: null }];
  const onSetPinned = vi.fn(() => Promise.reject(new ApiError({ kind: 'http', status: 409, code: 'conflict',
    message: 'A managed track cannot be pinned.' })));
  render(<UiPreferencesProvider preferences={createUiPreferences()}>
    <ThemeProvider storage={{ getItem: () => theme, setItem: () => undefined }}>
      <div className={`${styles.shell} ${styles.shellExpanded}`} style={{ blockSize: '100dvh' }}>
        <Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])} tracks={tracks}
          currentPath="/" onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
          onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={onSetPinned} onDeleteTrack={vi.fn()}
          onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()}
          collapsed={false} onToggleCollapsed={vi.fn()} />
        <main />
      </div>
    </ThemeProvider>
  </UiPreferencesProvider>);

  await page.getByRole('button', { name: /^Track Next task/ }).hover();
  await page.getByRole('button', { name: 'Pin Next task' }).click();
  const alert = page.getByRole('alert');
  await expect.element(alert).toBeVisible();
  await expect.element(alert.getByText('A managed track cannot be pinned.')).toBeVisible();
  await page.screenshot({ path: `test-results/operation-feedback-${theme}.png` });

  /* The banner treatment: a filled error surface with its glyph and a Dismiss, not bare text on the rail. */
  const element = alert.element();
  const filled = [element, ...element.querySelectorAll('*')].some((node) => getComputedStyle(node).backgroundColor !== 'rgba(0, 0, 0, 0)');
  expect(filled).toBe(true);
  expect(element.querySelector('svg')).not.toBeNull();
  await alert.getByRole('button', { name: /^Dismiss/ }).click();
  await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
});
