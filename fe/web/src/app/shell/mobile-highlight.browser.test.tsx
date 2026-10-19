import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { NEUTRAL_ACTIVITY } from '../../../../core/domain/track.ts';
import { MobileTrackList } from './mobile-track-list.tsx';
import { MobileHistory } from './mobile-history.tsx';
import { AreaSelector } from './area-selector.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function highlightRadius(host: HTMLElement) {
  const probe = document.createElement('span');
  probe.style.borderRadius = 'var(--radius-highlight)'; host.append(probe);
  const radius = getComputedStyle(probe).borderRadius; probe.remove(); return radius;
}

it.each(['light', 'dark'])('uses the highlight contour for selected and hovered mobile Tracks in %s', async theme => {
  await page.viewport(390, 844); document.documentElement.dataset.theme = theme;
  const view = render(<MobileTrackList tracks={[{ id: 'track', areaId: 'area', title: 'Selected track', sort: 0, cwd: '/tmp', isolation: 'none', agentCwd: '/tmp', pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0, ...NEUTRAL_ACTIVITY }]} currentTrackId="track" onOpenTrack={vi.fn()} isUnread={() => false} />);
  const track = view.getByRole('button', { name: 'Selected track' });
  expect(getComputedStyle(track).borderRadius).toBe(highlightRadius(view.container));
  expect(getComputedStyle(track.closest('li')!).borderRadius).toBe(highlightRadius(view.container));
});

it.each(['light', 'dark'])('uses the highlight contour for history views and rows in %s', async theme => {
  await page.viewport(390, 844); document.documentElement.dataset.theme = theme;
  const noop = vi.fn();
  const view = render(<MobileHistory open onOpenChange={noop} conversations={[{ id: 'chat', trackId: 'track', title: 'Selected conversation', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 0 }]} loading={false} failed={false} onRetry={noop} onNew={noop} onSelect={noop} onOpenSettings={noop} accountLabel="Account" accountInitial="A" onSignOut={noop} now={0} scopeLabel="Workspace scope" selectedConversationId="chat" trackGroups={{ pinned: [], unread: [], waiting: [] }} currentTrackId={undefined} onOpenTrack={noop} isUnread={() => false} tracksLoading={false} tracksError={null} onRetryTracks={noop} />);
  await page.getByRole('dialog', { name: '历史对话', exact: true }).findElement();
  for (const name of ['本 Track', 'Selected conversation']) {
    expect(getComputedStyle(view.getByRole('button', { name })).borderRadius).toBe(highlightRadius(view.container));
  }
  expect(getComputedStyle(view.getByRole('searchbox')).borderRadius).toBe('12px');
});

it.each(['light', 'dark'])('uses the highlight contour for the real Area menu in %s', async theme => {
  await page.viewport(390, 844); document.documentElement.dataset.theme = theme;
  const area = { id: 'area', name: 'Work', kind: 'user' as const, color: '#5B8DEF', sort: 0, defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0 };
  const view = render(<AreaSelector areas={[area]} activeArea={area} onSelectArea={vi.fn()} onCreateArea={vi.fn()} />);
  await page.getByRole('button', { name: 'Switch area, Work' }).click();
  const item = await page.getByRole('menuitem', { name: 'Work', exact: true }).findElement();
  expect(getComputedStyle(item).borderRadius).toBe(highlightRadius(view.container));
});
