import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../core/view/track-page.ts';
import { makeDesktopPainter, paintDesktopPanel } from '../../features/track/page/desktop-painter.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import { PanelCard } from '../../ui/panel-card/public.tsx';
import { SidebarFixture as Sidebar } from './sidebar-fixture.tsx';
import { MobileTrackList } from './mobile-track-list.tsx';
import { withFontRoleProbe } from '../../../../tools/styles/font-role-fixture.ts';
import { readReceiptsFixture } from '../providers/read-receipts-fixture.tsx';

const receipts = readReceiptsFixture();

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function draw(selected = false) {
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = ['Left first', 'Left second'].map((title, i) => ({
    id: `track-${i}`, areaId: area.id, title, sort: i, cwd: '/tmp', isolation: 'none', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY,
  }));
  const view = deriveTrackPageView({
    cards: ['Card first', 'Card second'].map((title, i) => ({
      id: `card-${i}`, track_id: 'track-0', title, kind: 'terminal', sort: i, payload: { terminal_presentation: { source: 'manual', in_workbench: true } },
      deletable: true, created_at: 1, updated_at: 1,
      runtime: { worker_session_id: `runtime-${i}`, kind: 'terminal', status: 'running' },
    })),
    tasks: ['Task first', 'Task second'].map((key, i) => ({
      blockId: `task-${i}`, key, state: 'ready', declaration: null, status: 'running', statusDetail: null,
      kind: 'codex', workerCardId: null, pendingReason: null,
    })),
    activity: NEUTRAL_ACTIVITY,
    openableCards: new Set(),
  });
  render(receipts.wrap(<div style={{ display: 'flex', gap: 32 }}>
    <div style={{ inlineSize: 240, background: 'var(--surface-navigation)' }}><Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])}
      tracks={tracks} currentPath={selected ? '/track/track-0' : '/'} onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
      onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={vi.fn()} onDeleteTrack={vi.fn()}
      onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} collapsed={false} onToggleCollapsed={vi.fn()} /></div>
    <div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ progressDisclosure: { open: true, onOpenChange: vi.fn() }, taskSummary: '2',
      cardEntries: <ChatList activeId={selected ? 'chat-0' : null} showTrack={false} onOpen={vi.fn()} cards={{}}
        conversations={['Chat first', 'Chat second'].map((title, i) => ({ provider: 'codex' as const, id: `chat-${i}`, trackId: 'track-0', title,
          kind: 'track-assistant', state: 'idle', updatedAt: 1 }))} />,
    }), { ...view, rowModules: view.desktop.rowModules })}</PanelCard></div>
  </div>));
  return { tracks };
}
function element(node: HTMLElement | null) {
  expect(node).not.toBeNull();
  return node!;
}
function typography(node: HTMLElement) {
  const style = getComputedStyle(node);
  return [style.fontFamily, style.fontSize, style.fontWeight, style.lineHeight, style.letterSpacing, style.color];
}

it.each(['light', 'dark'])('uses the same named type hierarchy in both %s side panels', async theme => {
  await page.viewport(1400, 900);
  draw(); document.documentElement.dataset.theme = theme;
  const name = element(document.querySelector<HTMLElement>('[title="Left first"]'));
  const primary = [element(document.querySelector<HTMLElement>('[data-nc-module="cards"] [data-nc-field="title"]')),
    element(document.querySelector<HTMLElement>('[data-nc-module="work"] [data-nc-field="title"]'))];
  const assertRole = (node: HTMLElement, role: Parameters<typeof withFontRoleProbe>[0]) => {
    withFontRoleProbe(role, document.documentElement, expected => {
      expect(getComputedStyle(node)).toMatchObject(expected);
    });
  };
  assertRole(name, '--type-content');
  for (const node of primary) assertRole(node, '--type-content');
  const group = element(document.querySelector<HTMLElement>('[title="Work"]'));
  assertRole(group, '--type-content');
  const inventoryGroup = element(document.querySelector<HTMLElement>('[data-nc-work-inventory] summary > span:first-child'));
  assertRole(inventoryGroup, '--type-metadata');
  expect(typography(element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"] > span > span')))).toEqual(typography(primary[0]));
  const workbenchTitle = element(document.querySelector<HTMLElement>('section[aria-label="Workbench"] h2'));
  const progressTitle = element(document.querySelector<HTMLElement>('[data-nc-work-inventory] summary > span:first-child'));
  expect(workbenchTitle.textContent).toBe('Workbench');
  expect(progressTitle.textContent).toBe('Progress');
  assertRole(workbenchTitle, '--type-metadata');
  assertRole(progressTitle, '--type-metadata');
  assertRole(element(document.querySelector<HTMLElement>('[title="Areas"]')), '--type-metadata');

});

it('uses configured sidebar density and uses compact single-line contextual tool entries', async () => {
  await page.viewport(1400, 900); draw();
  const area = element(document.querySelector<HTMLElement>('[aria-label="Collapse area Work"]'));
  const rows = [
    element(document.querySelector<HTMLElement>('[aria-label^="Track Left first"]')),
    element(document.querySelector<HTMLElement>('[data-nc-module="cards"] [data-nc-row]')),
    element(document.querySelector<HTMLElement>('[data-nc-module="work"] [data-nc-row]')),
    element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"]')),
    element(document.querySelector<HTMLElement>('[data-nc-work-inventory] summary')),
  ];
  for (const [index, row] of rows.entries()) expect(row.getBoundingClientRect().height).toBeCloseTo(index === 0 ? 32 : 28, 0);
  expect(area.getBoundingClientRect().height).toBeCloseTo(32, 0);
  const sidebarHeader = element(document.querySelector<HTMLElement>('nav[aria-label="Workspace"] h2')).parentElement!;
  expect(area.getBoundingClientRect().top - sidebarHeader.getBoundingClientRect().bottom).toBeCloseTo(0, 0);
  const panelHeader = element(document.querySelector<HTMLElement>('[data-nc-module="cards"] section[aria-label="Workbench"] h2')).parentElement!;
  expect(element(document.querySelector<HTMLElement>('[data-nc-module="cards"] [data-nc-card-inventory]')).getBoundingClientRect().top
    - panelHeader.getBoundingClientRect().bottom).toBeCloseTo(0, 0);

  expect(element(document.querySelector<HTMLElement>('[title="Work"]')).getBoundingClientRect().left).toBeCloseTo(element(document.querySelector<HTMLElement>('[title="Left first"]')).getBoundingClientRect().left, 1);
  const pairs = [
    [element(document.querySelector<HTMLElement>('[aria-label^="Track Left first"]')), element(document.querySelector<HTMLElement>('[aria-label^="Track Left second"]'))],
    [element(document.querySelector<HTMLElement>('[data-nc-row="tool:card-0"]')), element(document.querySelector<HTMLElement>('[data-nc-row="tool:card-1"]'))],
    [element(document.querySelector<HTMLElement>('[data-nc-row="task:task-0"]')), element(document.querySelector<HTMLElement>('[data-nc-row="task:task-1"]'))],
    [element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"]')), element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat second"]'))],
  ];
  for (const [index, [first, second]] of pairs.entries()) expect(second.getBoundingClientRect().top - first.getBoundingClientRect().top).toBeCloseTo(index === 0 ? 32 : 28, 0);

});

// The same selected state must paint identically over navigation and card surfaces.
it.each(['light', 'dark'])('shares opaque selection colours across both lists (%s)', async theme => {
  document.documentElement.dataset.theme = theme;
  await page.viewport(1280, 900);
  const { tracks } = draw(true);
  const mobileView = render(<MobileTrackList tracks={tracks} currentTrackId={tracks[0].id}
    onOpenTrack={vi.fn()} isUnread={() => false} />);
  const mobile = element(mobileView.container.querySelector<HTMLElement>('[aria-current="page"]'));
  const mobileRow = mobile.closest('li')!;
  const rail = element(document.querySelector<HTMLElement>('[aria-label^="Track Left first"]'));
  const chat = element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"]'));
  const probe = render(<div />).container.firstElementChild as HTMLDivElement;
  const expected = (token: string) => { probe.style.background = `var(${token})`; return getComputedStyle(probe).backgroundColor; };
  const paint = (node: HTMLElement, pseudo?: string) => getComputedStyle(node, pseudo).backgroundColor;
  await page.elementLocator(document.body).hover();
  const selected = expected('--selection-bg');
  await expect.poll(() => paint(rail, '::before')).toBe(selected);
  await expect.poll(() => paint(chat)).toBe(selected);
  expect(paint(mobileRow)).toBe(selected);
  expect(selected).not.toBe('rgba(0, 0, 0, 0)');
  expect(selected).not.toMatch(/rgba\(|\//);
  const selectedHover = expected('--selection-hover-bg');
  await page.elementLocator(rail).hover();
  await expect.poll(() => paint(rail, '::before')).toBe(selectedHover);
  await page.elementLocator(chat).hover();
  await expect.poll(() => paint(chat)).toBe(selectedHover);
  expect(getComputedStyle(chat).backgroundImage).toBe('none');
  await page.elementLocator(mobile).hover();
  await expect.poll(() => paint(mobileRow)).toBe(selectedHover);
  expect(paint(mobile)).toBe('rgba(0, 0, 0, 0)');
});
