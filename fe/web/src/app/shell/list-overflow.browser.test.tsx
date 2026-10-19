import { useState } from '../../ui/state/public.ts';
import { act, cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { SidebarTrackGroup } from './sidebar-group.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

async function frames() {
  for (let i = 0; i < 4; i++) await act(async () => {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
}

it('keeps unchanged Track title observers and measurements out of a real row selection', async () => {
  await page.viewport(1400, 900);
  const tracks: Track[] = Array.from({ length: 5 }, (_, index) => ({
    id: `track-${index}`, title: `Track title ${index}`, areaId: 'work', sort: index,
    cwd: '/workspace', isolation: 'none', agentCwd: '/workspace', pinnedAt: null, closedAt: null,
    createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY,
  }));
  const unchanged = new Set<Element>();
  let observations = 0, measurements = 0;
  const NativeResizeObserver = ResizeObserver;
  class TrackingResizeObserver extends NativeResizeObserver {
    override observe(target: Element, options?: ResizeObserverOptions) {
      if (unchanged.has(target)) observations++;
      super.observe(target, options);
    }
  }
  vi.stubGlobal('ResizeObserver', TrackingResizeObserver);
  const nativeWidth = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollWidth')!;
  vi.spyOn(Element.prototype, 'scrollWidth', 'get').mockImplementation(function (this: Element) {
    if (unchanged.has(this)) measurements++;
    return nativeWidth.get!.call(this) as number;
  });
  function Scene() {
    const [active, setActive] = useState(tracks[0].id);
    return <div style={{ inlineSize: 240 }}><SidebarTrackGroup emptyState={null} title="Work" label="area Work" level="area" expanded onToggle={() => {}}
      tracks={tracks} activeTrackId={active} onGo={target => { if (target.name === 'track') setActive(target.trackId); }}
      management={{ menuLabel: 'Group actions', canMoveUp: false, canMoveDown: false, onMove: () => {}, onHide: () => {} }}
      isUnread={() => false} onSetPinned={() => {}} onDelete={() => {}}
      trackActions={() => ({ areaPinned: false, onSetPinned: () => {}, onSetAreaPinned: () => {}, onMarkUnread: () => {} })} /></div>;
  }
  render(<Scene />);
  await frames();
  for (const node of document.querySelectorAll('[title]')) {
    if (['Track title 3', 'Track title 4'].includes(node.getAttribute('title')!)) unchanged.add(node);
  }
  expect(unchanged.size).toBe(2);
  await page.getByRole('button', { name: 'Track Track title 1', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Track Track title 1', exact: true })).toHaveAttribute('aria-current', 'page');
  await frames();
  expect(observations).toBe(0);
  expect(measurements).toBe(0);
});
