import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); });

function element(node: HTMLElement | null) {
  expect(node).not.toBeNull();
  return node!;
}

/* `Show N more` is a row in the Track list, not a footer: same height, same text column. */
it('puts Show N more on the Track row rhythm, in the Track title column', async () => {
  await page.viewport(1400, 900);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = Array.from({ length: 7 }, (_, i) => ({
    id: `track-${i}`, areaId: area.id, title: `Row ${i + 1}`, sort: i, lifecycle: 'draft', cwd: '/tmp', agentCwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY,
  }));
  render(<div style={{ inlineSize: 240 }}><Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])}
    tracks={tracks} currentPath="/" onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
    onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={vi.fn()} onDeleteTrack={vi.fn()}
    onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} collapsed={false} onToggleCollapsed={vi.fn()} /></div>);

  const more = element(document.querySelector<HTMLElement>('[aria-label="Show 2 more in Work"]'));
  const lastRow = element(document.querySelector<HTMLElement>('[aria-label^="Track Row 5"]'));
  const lastTitle = element(document.querySelector<HTMLElement>('[title="Row 5"]'));
  expect(more.getBoundingClientRect().height).toBeCloseTo(lastRow.getBoundingClientRect().height, 0);
  expect(more.getBoundingClientRect().top - lastRow.getBoundingClientRect().top).toBeCloseTo(28, 0);
  const range = document.createRange();
  range.selectNodeContents(more);
  expect(range.getBoundingClientRect().left).toBeCloseTo(lastTitle.getBoundingClientRect().left, 0);
});
