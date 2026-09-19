import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../core/view/track-page.ts';
import { makeDesktopPainter, paintDesktopPanel } from '../../features/track/page/desktop-painter.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import { PanelCard, PanelModule } from '../../ui/panel-card/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function draw() {
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = ['Left first', 'Left second'].map((title, i) => ({
    id: `track-${i}`, areaId: area.id, title, sort: i, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY,
  }));
  const view = deriveTrackPageView({
    cards: ['Card first', 'Card second'].map((title, i) => ({
      id: `card-${i}`, track_id: 'track-0', title, kind: 'terminal', sort: i, payload: null,
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
  render(<div style={{ display: 'flex', gap: 32 }}>
    <div style={{ inlineSize: 240 }}><Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])}
      tracks={tracks} currentPath="/" onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
      onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={vi.fn()} onDeleteTrack={vi.fn()}
      onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} collapsed={false} onToggleCollapsed={vi.fn()} /></div>
    <div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ taskSummary: '2' }), view)}
      <PanelModule title="Conversations"><ChatList showTrack={false} onOpen={vi.fn()} cards={{}}
        conversations={['Chat first', 'Chat second'].map((title, i) => ({ id: `chat-${i}`, trackId: 'track-0', title,
          kind: 'track-assistant', state: 'idle', updatedAt: 1 }))} /></PanelModule>
    </PanelCard></div>
  </div>);
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
    element(document.querySelector<HTMLElement>('[data-nc-module="tasks"] [data-nc-field="title"]'))];
  expect(getComputedStyle(name).fontSize).toBe('13px');
  expect(Number.parseFloat(getComputedStyle(name).lineHeight)).toBeCloseTo(16.9, 1);
  for (const node of primary) expect(typography(node)).toEqual(typography(name));
  const group = element(document.querySelector<HTMLElement>('[title="Work"]'));
  expect(getComputedStyle(group).fontWeight).toBe('500');
  expect(getComputedStyle(group).fontSize).toBe('13px');
  expect(typography(element(document.querySelector<HTMLElement>('[data-nc-inventory-group] summary > span:first-child')))).toEqual(typography(group));
  expect(typography(element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"] > span > span')))).toEqual(typography(group));
  expect(typography(element(document.querySelector<HTMLElement>('[data-nc-module] h2')))).toEqual(typography(element(document.querySelector<HTMLElement>('nav[aria-label="Workspace"] h2'))));
  const panelTitles = [...document.querySelectorAll<HTMLElement>('h2')].filter(node => ['Cards', 'Tasks', 'Conversations'].includes(node.textContent ?? ''));
  expect(panelTitles).toHaveLength(3);
  for (const title of panelTitles) expect(typography(title)).toEqual(typography(panelTitles[0]));

});

it('uses a 28px row rhythm, an aligned group edge and matching section spacing', async () => {
  await page.viewport(1400, 900); draw();
  const area = element(document.querySelector<HTMLElement>('[aria-label="Collapse area Work"]'));
  const rows = [
    element(document.querySelector<HTMLElement>('[aria-label^="Track Left first"]')),
    element(document.querySelector<HTMLElement>('[data-nc-module="cards"] [data-nc-row]')),
    element(document.querySelector<HTMLElement>('[data-nc-module="tasks"] [data-nc-row]')),
    element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"]')),
    element(document.querySelector<HTMLElement>('[data-nc-inventory-group] summary')),
  ];
  for (const row of rows) expect(row.getBoundingClientRect().height).toBeCloseTo(28, 0);
  expect(area.getBoundingClientRect().height).toBeCloseTo(28, 0);
  const sidebarHeader = element(document.querySelector<HTMLElement>('nav[aria-label="Workspace"] h2')).parentElement!;
  expect(area.getBoundingClientRect().top - sidebarHeader.getBoundingClientRect().bottom).toBeCloseTo(4, 0);
  const panelHeader = element(document.querySelector<HTMLElement>('[data-nc-module="cards"] h2')).parentElement!;
  expect(element(document.querySelector<HTMLElement>('[data-nc-module="cards"] [data-nc-inventory-group]')).getBoundingClientRect().top
    - panelHeader.getBoundingClientRect().bottom).toBeCloseTo(4, 0);

  expect(element(document.querySelector<HTMLElement>('[title="Work"]')).getBoundingClientRect().left).toBeCloseTo(element(document.querySelector<HTMLElement>('[title="Left first"]')).getBoundingClientRect().left, 0);
  const pairs = [
    [element(document.querySelector<HTMLElement>('[aria-label^="Track Left first"]')), element(document.querySelector<HTMLElement>('[aria-label^="Track Left second"]'))],
    [element(document.querySelector<HTMLElement>('[data-nc-row="card-0"]')), element(document.querySelector<HTMLElement>('[data-nc-row="card-1"]'))],
    [element(document.querySelector<HTMLElement>('[data-nc-row="task-0"]')), element(document.querySelector<HTMLElement>('[data-nc-row="task-1"]'))],
    [element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat first"]')), element(document.querySelector<HTMLElement>('[aria-label="Conversation Chat second"]'))],
  ];
  for (const [first, second] of pairs) expect(second.getBoundingClientRect().top - first.getBoundingClientRect().top).toBeCloseTo(28, 0);

});
