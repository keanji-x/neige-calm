import { cleanup, fireEvent, render } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import type { ReportTaskRow } from '../../../../core/domain/report.ts';
import { ChatList } from '../../features/chat/list/public.tsx';
import { RecentFiles } from '../../features/report/recent-files/public.tsx';
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { AddCardMenu } from '../../features/track/new-card/public.tsx';
import { manualCard as card, renderPage, track } from '../../features/track/page/test-fixtures.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });
const task = (blockId: string, status: string, workerCardId: string | null): ReportTaskRow => ({
  blockId, key: blockId, state: 'ready', declaration: null, status, statusDetail: null,
  kind: 'codex', workerCardId, pendingReason: null,
});

it('keeps execution details and related entries in one card with single-line rows', async () => {
  await page.viewport(1400, 900);
  const openTask = vi.fn(); const openCard = vi.fn(); const deleteCard = vi.fn();
  const openFile = vi.fn(); const openSource = vi.fn(); const openChat = vi.fn();
  renderPage({
    track: track({ cards: { worker: 'input' } }),
    cards: [card({ id: 'worker', kind: 'codex', title: 'Worker session' }),
      card({ id: 'server', title: 'Development server' })],
    tasks: [task('Review layout', 'running', 'worker'), task('Finished review', 'done', null)],
    onOpenTask: openTask, onOpenCard: openCard, onDeleteCard: deleteCard,
    conversationList: <ChatList context cards={{}} showTrack={false} onOpen={openChat} conversations={[
      { id: 'planner', provider: 'codex', trackId: 'w1', title: 'Planner', kind: 'track-assistant', state: 'idle', updatedAt: 1 },
    ]} />,
    recentFiles: <RecentFiles inline paths={['docs/notes.md']} onOpen={openFile} />,
    backlinks: <ReportBacklinks inline trackId="w1" onOpen={openSource} backlinks={{
      backlinks: [{ src_track_id: 'source', src_track_title: 'Design notes', src_block_id: 'citation', label: 'Layout', updated_at: 1 }],
      truncated: true, skipped_sources: 1,
    }} />,
  });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  expect(root.children).toHaveLength(1);
  const module = root.querySelector<HTMLElement>('[data-nc-module="work"]')!;
  const frame = module.parentElement!;
  expect(getComputedStyle(frame).paddingTop).toBe('12px');
  expect(getComputedStyle(frame).paddingInlineStart).toBe('12px');
  const progress = root.querySelector<HTMLElement>('section[aria-label="Progress"]')!;
  expect(progress.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  expect(progress.getBoundingClientRect().top - root.querySelector('section[aria-label="Conversations"]')!.getBoundingClientRect().bottom).toBe(12);
  await userEvent.click(progress.querySelector('button[aria-expanded]')!);
  for (const heading of root.querySelectorAll<HTMLElement>('section[aria-label] h2')) {
    expect(getComputedStyle(heading)).toMatchObject({ fontSize: '12px', lineHeight: '16px', fontWeight: '500' });
    expect(heading.parentElement!.getBoundingClientRect().height).toBe(28);
  }
  expect([...root.querySelectorAll('section[aria-label]')].map(section => section.getAttribute('aria-label')))
    .toEqual(['Conversations', 'Progress', 'Workbench', 'Related']);
  expect(root.querySelector('[data-nc-row="tool:worker"]')).toBeNull();
  const work = root.querySelector<HTMLElement>('[data-nc-row="task:Review layout"]')!;
  expect(work.getBoundingClientRect().height).toBe(28);
  expect(work.querySelectorAll('[data-nc-status]')).toHaveLength(1);
  expect(work.querySelector<HTMLElement>('[data-nc-status]')!.closest<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
  await userEvent.click(work.querySelector<HTMLButtonElement>('[data-nc-row-action="reveal-block"]')!);
  expect(openTask).toHaveBeenCalledWith('Review layout');
  const menu = work.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]')!;
  menu.focus();
  expect(getComputedStyle(menu.parentElement!).opacity).toBe('1');
  await userEvent.keyboard('{Enter}');
  await page.getByRole('menuitem', { name: 'View execution', exact: true }).click();
  expect(openCard).toHaveBeenCalledWith('worker');
  await userEvent.click(menu);
  await page.getByRole('menuitem', { name: 'Delete card Worker session', exact: true }).click();
  expect(deleteCard).toHaveBeenCalledWith('worker', {
    kind: 'anchor', anchor: { ref: { current: work } }, unavailable: 'surface-center',
  });
  await page.getByRole('button', { name: 'Conversation Planner', exact: true }).click();
  expect(openChat).toHaveBeenCalledOnce();
  const related = root.querySelector<HTMLElement>('section[aria-label="Related"]')!;
  expect(related.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  await userEvent.click(related.querySelector('button[aria-expanded]')!);
  await page.getByRole('button', { name: 'Open docs/notes.md', exact: true }).click();
  expect(openFile).toHaveBeenCalledWith('docs/notes.md');
  await page.getByRole('button', { name: 'Referenced by Design notes', exact: true }).click();
  expect(openSource).toHaveBeenCalledWith('source', 'citation');
  await expect.element(page.getByText('Some backlinks are not shown.')).toBeVisible();
  await expect.element(page.getByText('1 source report could not be read.')).toBeVisible();
  for (const row of related.querySelectorAll('button')) expect(row.getBoundingClientRect().height).toBe(28);
  await page.getByRole('button', { name: 'Finished review', exact: true }).click();
  expect(openTask).toHaveBeenLastCalledWith('Finished review');
});

it('retains finished-worker activity and standalone failure inside folded progress', async () => {
  await page.viewport(1400, 900);
  renderPage({ track: track({ cards: { worker: 'input' } }),
    cards: [card({ id: 'worker', title: 'Worker' }), card({ id: 'failed', title: 'Build log',
      runtime: { worker_session_id: 'failed-session', kind: 'terminal', status: 'failed' } })],
    tasks: [task('Confirm result', 'done', 'worker')],
  });
  const attention = document.querySelector<HTMLElement>('section[aria-label="Progress"]')!;
  expect(attention.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  await userEvent.click(attention.querySelector('button[aria-expanded]')!);
  expect([...attention.querySelectorAll('[data-nc-row]')].map(row => row.getAttribute('data-nc-row')))
    .toEqual(['task:Confirm result']);
  expect([...document.querySelectorAll('[data-nc-card-inventory] [data-nc-row]')].map(row => row.getAttribute('data-nc-row'))).toEqual(['tool:failed']);
  expect(attention.querySelectorAll('details')).toHaveLength(0);
  expect(page.getByRole('button', { name: 'Build log', exact: true }).element().getAttribute('aria-description')).toBe('session failed');
});

it('bounds long work and context lists while keeping the last entry operable', async () => {
  await page.viewport(1400, 900);
  const openTask = vi.fn();
  renderPage({ tasks: Array.from({ length: 40 }, (_, index) => task(`Task ${index}`, 'running', null)), onOpenTask: openTask,
    recentFiles: <RecentFiles inline paths={Array.from({ length: 30 }, (_, index) => `file-${index}.md`)} onOpen={vi.fn()} /> });
  const panel = document.querySelector<HTMLElement>('[data-nc-panel]')!;
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const list = document.querySelector<HTMLElement>('[data-nc-work-inventory] ul')!;
  expect(getComputedStyle(list).overflowY).toBe('auto');
  expect(list.clientHeight).toBeLessThanOrEqual(224);
  expect(list.scrollHeight).toBeGreaterThan(list.clientHeight);
  list.scrollTop = list.scrollHeight;
  await page.getByRole('button', { name: 'Task 39', exact: true }).click();
  expect(openTask).toHaveBeenCalledWith('Task 39');
  await page.getByRole('button', { name: 'Related', exact: true }).click();
  expect(panel.scrollHeight).toBeGreaterThan(panel.clientHeight);
  const files = document.querySelector<HTMLElement>('[data-nc-recent-files]')!;
  expect(getComputedStyle(files.parentElement!).overflowY).toBe('visible');
  await page.getByRole('button', { name: 'Open file-29.md', exact: true }).click();
});

it('keeps empty groups hidden while retaining the single card-level creation action', async () => {
  await page.viewport(1400, 900);
  renderPage({ cardsAction: <button type="button">New</button>, tasks: [task('Old task', 'done', null)],
    recentFiles: <RecentFiles inline paths={['notes.md']} onOpen={vi.fn()} /> });
  const root = document.querySelector('[data-nc-desktop-panel]')!;
  expect(root.querySelector('section[aria-label="Current"]')).toBeNull();
  expect(root.querySelector('section[aria-label="Needs attention"]')).toBeNull();
  const creation = page.getByRole('button', { name: 'New', exact: true });
  await creation.click();
  expect(root.querySelectorAll('button[aria-expanded="false"]')).toHaveLength(2);
});

it('uses the existing unknown-kind fallback and preserves selection while focusing a conversation', async () => {
  await page.viewport(1400, 900);
  renderPage({ cards: [card({ id: 'plugin', kind: 'future-tool', title: 'Custom tool' })],
    conversationContextList: <ChatList context cards={{}} showTrack={false} activeId="planner" onOpen={vi.fn()} conversations={[
      { id: 'planner', provider: 'codex', trackId: 'w1', title: 'Planner', kind: 'track-assistant', state: 'idle', updatedAt: 1 },
    ]} />,
  });
  expect(document.querySelector('[data-nc-row="tool:plugin"] svg')!.classList.contains('lucide-file')).toBe(true);
  const conversation = page.getByRole('button', { name: 'Conversation Planner', exact: true }).element() as HTMLButtonElement;
  conversation.focus();
  await userEvent.keyboard('{Tab}');
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  expect(document.activeElement).toBe(conversation);
  const probe = document.createElement('div'); probe.style.backgroundColor = 'var(--selection-hover-bg)';
  document.body.append(probe);
  expect(getComputedStyle(conversation.parentElement!).backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
  expect(getComputedStyle(conversation).outlineStyle).not.toBe('none');
  probe.remove();
});

it('pins conversations first with a persistent creation action and does not repeat them in status groups', async () => {
  await page.viewport(1400, 900);
  const create = vi.fn();
  renderPage({ onStartConversation: create,
    tasks: [task('Review result', 'needs_input', null)],
    conversationContextList: <ChatList context cards={{ planner: 'input' }} showTrack={false} onOpen={vi.fn()} conversations={[
      { id: 'planner', provider: 'codex', trackId: 'w1', title: 'Planner', kind: 'track-assistant', state: 'running', updatedAt: 1 },
    ]} />,
  });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const groups = [...root.querySelectorAll('section[aria-label]')];
  expect(groups.map(group => group.getAttribute('aria-label'))).toEqual(['Conversations', 'Progress', 'Workbench']);
  const button = page.getByRole('button', { name: 'New conversation', exact: true }).element() as HTMLButtonElement;
  await userEvent.hover(document.body);
  expect(button.checkVisibility({ checkOpacity: true })).toBe(true);
  await userEvent.click(button);
  expect(create).toHaveBeenCalledOnce();
  expect(root.querySelectorAll('[aria-label="Conversation Planner"]')).toHaveLength(1);
  expect(groups[0].querySelector('[data-nc-activity="attention"]')).not.toBeNull();
  expect(root.querySelector('[data-nc-work-inventory]')!.textContent).not.toContain('Planner');
  expect(root.querySelector('[data-nc-work-inventory] button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
});

it('aligns header controls, disclosure arrows and row menus on one system action axis', async () => {
  await page.viewport(1400, 900);
  renderPage({ onStartConversation: vi.fn(), onDeleteCard: vi.fn(),
    cardsAction: <AddCardMenu label="Tools" icon="more" entries={[]} onSelect={vi.fn()} />,
    tasks: [task('Active', 'needs_input', null), task('Old', 'done', null)],
    cards: [card({ id: 'server', title: 'Server' })],
    recentFiles: <RecentFiles inline paths={['notes.md']} onOpen={vi.fn()} /> });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const center = (element: Element) => { const box = element.getBoundingClientRect(); return { x: (box.left + box.right) / 2, y: (box.top + box.bottom) / 2 }; };
  const plus = page.getByRole('button', { name: 'New conversation', exact: true }).element();
  expect(plus.getBoundingClientRect().width).toBe(20);
  expect(plus.getBoundingClientRect().height).toBe(20);
  expect(getComputedStyle(plus).borderRadius).toBe('8px');
  expect(center(plus).y).toBe(center(plus.closest('section')!.querySelector('h2')!.parentElement!).y);
  const axis = center(plus.querySelector('svg')!).x;
  const tools = page.getByRole('button', { name: 'Tools', exact: true }).element();
  expect(tools.querySelector('svg')!.getBoundingClientRect().width).toBe(14);
  expect(center(tools.querySelector('svg')!).y).toBe(center(tools.closest('section[aria-label="Workbench"]')!.querySelector('h2')!.parentElement!).y);
  for (const summary of root.querySelectorAll('button[aria-expanded][aria-controls]:not([aria-haspopup])')) expect(center([...summary.querySelectorAll('svg')].at(-1)!).x).toBe(axis);
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const menu = page.getByRole('button', { name: 'Actions for Server', exact: true }).element();
  expect(center(menu.querySelector('svg')!).x).toBe(axis);
  const conversation = root.querySelector<HTMLElement>('section[aria-label="Conversations"]')!;
  const progress = root.querySelector<HTMLElement>('[data-nc-work-inventory] section[aria-label="Progress"]')!;
  expect(progress.getBoundingClientRect().top - conversation.getBoundingClientRect().bottom).toBe(12);
  const work = root.querySelector<HTMLElement>('[data-nc-module="work"]')!;
  const related = root.querySelector<HTMLElement>('section[aria-label="Related"]')!;
  const workbench = root.querySelector<HTMLElement>('section[aria-label="Workbench"]')!;
  expect(workbench.getBoundingClientRect().top - work.getBoundingClientRect().bottom).toBe(12);
  expect(related.getBoundingClientRect().top - workbench.querySelector('[data-nc-card-inventory]')!.getBoundingClientRect().bottom).toBe(12);
});

it('does not leave an empty work spacer between conversations and related content', async () => {
  await page.viewport(1400, 900);
  renderPage({ onStartConversation: vi.fn(), recentFiles: <RecentFiles inline paths={['notes.md']} onOpen={vi.fn()} /> });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const conversation = root.querySelector<HTMLElement>('section[aria-label="Conversations"]')!;
  const related = root.querySelector<HTMLElement>('section[aria-label="Related"]')!;
  const workbench = root.querySelector<HTMLElement>('section[aria-label="Workbench"]')!;
  expect(workbench.getBoundingClientRect().top - conversation.getBoundingClientRect().bottom).toBe(12);
  expect(related.getBoundingClientRect().top - workbench.querySelector('[data-nc-card-inventory]')!.getBoundingClientRect().bottom).toBe(12);
});


it('distinguishes recent files from incoming references in single-line related entries', async () => {
  await page.viewport(1400, 900);
  const path = 'docs/a-long-directory/context-card-information-and-layout-notes.md';
  const source = 'A long source page title about the context card information hierarchy';
  const openFile = vi.fn(); const openSource = vi.fn();
  renderPage({ recentFiles: <RecentFiles inline paths={[path]} onOpen={openFile} />,
    backlinks: <ReportBacklinks inline trackId="w1" onOpen={openSource} backlinks={{
      backlinks: [{ src_track_id: 'source', src_track_title: source, src_block_id: 'citation', label: 'Layout', updated_at: 1 }],
      truncated: false, skipped_sources: 0,
    }} />,
  });
  await page.getByRole('button', { name: 'Related', exact: true }).click();
  const file = page.getByRole('button', { name: `Open ${path}`, exact: true }).element();
  const reference = page.getByRole('button', { name: `Referenced by ${source}`, exact: true }).element();
  for (const [row, relation] of [[file, 'Recent'], [reference, 'References this']] as const) {
    const metadata = [...row.querySelectorAll('span')].find(span => span.textContent === relation)!;
    expect(metadata).toBeDefined();
    expect(metadata.getBoundingClientRect().height).toBe(16);
    expect(row.getBoundingClientRect().height).toBe(28);
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth);
    const label = row.querySelector('svg + span')!;
    expect(label.getBoundingClientRect().width).toBeGreaterThan(100);
    expect(label.getBoundingClientRect().right).toBeLessThan(metadata.getBoundingClientRect().left);
  }
  expect(file.querySelector('svg')!.classList.contains('lucide-file')).toBe(true);
  expect(reference.querySelector('svg')!.classList.contains('lucide-link2')).toBe(true);
  expect(file.getAttribute('title')).toContain(path);
  await userEvent.click(file); expect(openFile).toHaveBeenCalledWith(path);
  await userEvent.click(reference); expect(openSource).toHaveBeenCalledWith('source', 'citation');
});


it('keeps Planner-managed execution folded without presenting worker failure as a user task', async () => {
  await page.viewport(1400, 900);
  const openTask = vi.fn();
  renderPage({ onStartConversation: vi.fn(), onOpenTask: openTask,
    tasks: [task('Check layout', 'failed', 'worker'), task('Update tokens', 'running', null)],
    cards: [card({ id: 'worker', title: 'Layout worker', runtime: { worker_session_id: 'session', kind: 'terminal', status: 'failed' } })],
    track: track({ cards: { worker: 'failed' } }),
    conversationContextList: <ChatList context cards={{}} showTrack={false} onOpen={vi.fn()} conversations={[
      { id: 'planner', provider: 'codex', trackId: 'w1', title: 'Planner', kind: 'track-assistant', state: 'idle', updatedAt: 1 },
    ]} />,
  });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  expect(root.querySelector('section[aria-label="Needs attention"]')).toBeNull();
  expect(root.querySelector('section[aria-label="Current"]')).toBeNull();
  const progress = root.querySelector<HTMLElement>('section[aria-label="Progress"]')!;
  expect(progress).toBeDefined(); expect(progress.querySelector('button[aria-expanded]')?.getAttribute('aria-expanded')).toBe('false');
  await expect.element(page.getByRole('button', { name: 'Conversation Planner', exact: true })).toBeVisible();
  expect(root.querySelector('[data-nc-row="tool:worker"]')).toBeNull();
  expect(root.querySelector('[data-nc-row="task:Check layout"]')).toBeNull();
  await userEvent.click(progress.querySelector('button[aria-expanded]')!);
  const row = root.querySelector<HTMLElement>('[data-nc-row="task:Check layout"]')!;
  expect(row.checkVisibility()).toBe(true);
  expect(row.querySelector('button')!.getAttribute('aria-description')).toBe('failed');
  await userEvent.click(row.querySelector('button')!);
  expect(openTask).toHaveBeenCalledWith('Check layout');
});


it.each([
  ['failed', 'Execution failed'],
  ['input', 'Execution awaiting input'],
] as const)('describes background %s without turning it into a user request', async (activity, description) => {
  await page.viewport(1400, 900);
  renderPage({ track: track({ cards: { worker: activity } }),
    tasks: [task('Inspect layout', 'done', 'worker')], cards: [card({ id: 'worker', title: 'Layout worker' })],
  });
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const row = document.querySelector('[data-nc-row="task:Inspect layout"]')!;
  const control = row.querySelector<HTMLButtonElement>('[data-nc-row-action="reveal-block"]')!;
  await expect.element(page.getByRole('button', { name: `Inspect layout ${description}`, exact: true })).toBeVisible();
  expect(control.getAttribute('aria-description')).toBe('done');
  expect(row.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity')).toBe(activity === 'input' ? 'attention' : 'failed');
  expect(row.textContent).not.toMatch(/Needs attention|Needs input/);
});


it('bounds long conversation and progress lists independently while retaining headers and last-row actions', async () => {
  await page.viewport(1400, 1000);
  const openChat = vi.fn(); const openTask = vi.fn(); const openWorker = vi.fn();
  renderPage({ onStartConversation: vi.fn(), onOpenTask: openTask, onOpenCard: openWorker,
    cards: Array.from({ length: 30 }, (_, index) => card({ id: `worker-${index}`, updated_at: 30 - index })),
    tasks: Array.from({ length: 30 }, (_, index) => task(`Work ${index}`, 'running', `worker-${index}`)),
    conversationContextList: <ChatList context cards={{}} showTrack={false} onOpen={openChat} conversations={
      Array.from({ length: 30 }, (_, index) => ({ id: `chat-${index}`, provider: 'codex' as const, trackId: 'w1',
        title: `Discussion ${index}`, kind: 'track-assistant' as const, state: 'idle' as const, updatedAt: index }))
    } />,
  });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const chats = root.querySelector<HTMLElement>('section[aria-label="Conversations"] ul')!;
  expect(chats.clientHeight).toBeLessThanOrEqual(224);
  expect(chats.scrollHeight).toBeGreaterThan(chats.clientHeight);
  const header = page.getByRole('button', { name: 'New conversation', exact: true }).element();
  const top = header.getBoundingClientRect().top;
  await page.getByRole('button', { name: 'Conversation Discussion 0', exact: true }).click();
  expect(openChat).toHaveBeenCalledOnce();
  expect(header.getBoundingClientRect().top).toBe(top);
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const progress = root.querySelector<HTMLElement>('[data-nc-work-inventory] ul')!;
  expect(progress.clientHeight).toBeLessThanOrEqual(224);
  expect(progress.scrollHeight).toBeGreaterThan(progress.clientHeight);
  const last = page.getByRole('button', { name: 'Work 29', exact: true }).element() as HTMLButtonElement;
  await userEvent.keyboard('{Tab}');
  last.focus();
  expect(document.activeElement).toBe(last);
  expect(last.matches(':focus-visible')).toBe(true);
  expect(progress.scrollTop).toBeGreaterThan(0);
  const focusedStyle = getComputedStyle(last);
  expect(focusedStyle.outlineStyle).toBe('solid');
  expect(parseFloat(focusedStyle.outlineWidth)).toBeGreaterThan(0);
  const ringExtent = parseFloat(focusedStyle.outlineWidth) + parseFloat(focusedStyle.outlineOffset);
  expect(last.getBoundingClientRect().left - ringExtent).toBeGreaterThanOrEqual(progress.getBoundingClientRect().left);
  expect(last.getBoundingClientRect().right + ringExtent).toBeLessThanOrEqual(progress.getBoundingClientRect().right);
  expect(last.getBoundingClientRect().bottom + ringExtent).toBeLessThanOrEqual(progress.getBoundingClientRect().bottom);
  await userEvent.keyboard('{Enter}');
  expect(openTask).toHaveBeenCalledWith('Work 29');
  await page.getByRole('button', { name: 'Actions for Work 29', exact: true }).click();
  await page.getByRole('menuitem', { name: 'View execution', exact: true }).click();
  expect(openWorker).toHaveBeenCalledWith('worker-29');
});

it('shows a compact real status and keeps finished work after newer running work', async () => {
  await page.viewport(1400, 900);
  renderPage({ cards: [card({ id: 'old', updated_at: 100 }), card({ id: 'new', updated_at: 200 }), card({ id: 'done', updated_at: 999 })],
    tasks: [task('Earlier work', 'running', 'old'), task('Latest work', 'running', 'new'), task('Finished work', 'done', 'done')],
  });
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const rows = [...document.querySelectorAll('[data-nc-work-inventory] [data-nc-row]')];
  expect(rows.map(row => row.getAttribute('data-nc-row'))).toEqual(['task:Latest work', 'task:Earlier work', 'task:Finished work']);
  expect(rows.map(row => row.querySelector('button > span:last-child')?.textContent)).toEqual(['Running', 'Running', 'Finished']);
  for (const row of rows) expect(row.getBoundingClientRect().height).toBe(28);
});


it('preserves a long unknown phase without overflowing the name or hiding its exact description', async () => {
  await page.viewport(1000, 900);
  const phase = 'future-provider-execution-phase-with-an-unusually-long-name';
  renderPage({ tasks: [task('Inspect a long task name in a narrow panel', phase, null)] });
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  const row = document.querySelector('[data-nc-work-inventory] [data-nc-row]')!;
  const button = row.querySelector('button')!;
  expect(button.scrollWidth).toBeLessThanOrEqual(button.clientWidth);
  expect(button.getAttribute('aria-description')).toBe(phase);
  expect(row.getAttribute('title')).toContain(phase);
  expect(row.querySelector('svg + span')!.getBoundingClientRect().width).toBeGreaterThan(60);
  expect(row.getBoundingClientRect().height).toBe(28);
});


it('right-aligns text states and centers conversation indicators on the same system end slot', async () => {
  await page.viewport(1400, 900);
  renderPage({ onStartConversation: vi.fn(), onDeleteCard: vi.fn(),
    cards: [card({ id: 'worker', title: 'Runtime' })],
    tasks: [task('Has menu', 'running', 'worker'), task('No menu', 'done', null)],
    conversationContextList: <ChatList context showTrack={false} onOpen={vi.fn()}
      cards={{ running: 'working' }} unreadIds={new Set(['unread'])} awaitingInputIds={new Set(['question'])}
      conversations={['running', 'unread', 'question', 'quiet'].map(id => ({ id, provider: 'codex', trackId: 'w1', title: id,
        kind: 'track-assistant', state: 'running', updatedAt: 1 }))} />,
  });
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const plus = page.getByRole('button', { name: 'New conversation', exact: true }).element();
  const axis = (plus.getBoundingClientRect().left + plus.getBoundingClientRect().right) / 2;
  for (const [name, state] of [['running', 'working'], ['unread', 'unread'], ['question', 'attention']] as const) {
    const row = page.getByRole('button', { name: `Conversation ${name}${state === 'working' ? ', working' : ''}`, exact: true }).element();
    const marker = [...row.querySelectorAll<HTMLElement>('[data-nc-activity]')].find(node => node.getAttribute('data-nc-activity') === state)!;
    expect(marker).not.toBeNull();
    expect((marker.getBoundingClientRect().left + marker.getBoundingClientRect().right) / 2).toBe(axis);
    expect(marker.checkVisibility({ checkOpacity: true })).toBe(true);
    if (state === 'working') expect(getComputedStyle(marker).animationName).not.toBe('none');
  }
  const quiet = page.getByRole('button', { name: 'Conversation quiet', exact: true }).element();
  expect(quiet.querySelector('[data-nc-activity]')).toBeNull();
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] button[aria-expanded]')!);
  await userEvent.hover(document.body);
  for (const id of ['Has menu', 'No menu']) {
    const row = [...root.querySelectorAll<HTMLElement>('[data-nc-row]')].find(node => node.getAttribute('data-nc-row') === `task:${id}`)!;
    const status = row.querySelector('button')!.lastElementChild!;
    expect(status.getBoundingClientRect().right).toBe(plus.getBoundingClientRect().right);
  }
  const menu = page.getByRole('button', { name: 'Actions for Has menu', exact: true }).element();
  const row = menu.closest('li')!;
  await userEvent.hover(row);
  expect(menu.checkVisibility({ checkOpacity: true })).toBe(true);
  expect((menu.getBoundingClientRect().left + menu.getBoundingClientRect().right) / 2).toBe(axis);
});


it.each([false, true])('shares Progress and Workbench press and disclosure motion with reduced motion=%s', async (reduced) => {
  await page.viewport(1400, 900);
  await commands.emulateReducedMotion(reduced);
  try {
    render(<DepthFeedback />);
    renderPage({ tasks: [task('Check unified motion', 'running', null)] });
    for (const title of ['Progress', 'Workbench']) {
      const toggle = page.getByRole('button', { name: title, exact: true }).element() as HTMLButtonElement;
      const body = document.getElementById(toggle.getAttribute('aria-controls')!)!;
      const wasOpen = toggle.getAttribute('aria-expanded') === 'true';
      const arrow = toggle.querySelector<HTMLElement>('[data-nc-spring-rotation]')!;
      expect(arrow).not.toBeNull();
      fireEvent.pointerDown(toggle, { pointerId: 1, isPrimary: true, button: 0 });
      expect(toggle.getAnimations()).toHaveLength(reduced ? 0 : 1);
      if (!reduced) {
        const press = toggle.getAnimations()[0];
        press.pause(); press.currentTime = 80;
        expect(new DOMMatrix(getComputedStyle(toggle).transform).a).toBeLessThan(1);
      }
      fireEvent.pointerUp(toggle, { pointerId: 1 });
      // The shared native button owns the entire header hit area.
      await userEvent.click(toggle);
      expect(toggle.getAttribute('aria-expanded')).toBe(String(!wasOpen));
      expect(body.hidden).toBe(wasOpen);
      await expect.poll(() => arrow.getAnimations().length).toBe(0);
      expect(getComputedStyle(arrow).rotate).toBe(wasOpen ? '0deg' : '90deg');
      toggle.focus();
      await userEvent.keyboard('{Enter}');
      expect(toggle.getAttribute('aria-expanded')).toBe(String(wasOpen));
      await userEvent.keyboard(' ');
      expect(toggle.getAttribute('aria-expanded')).toBe(String(!wasOpen));
      expect(document.activeElement).toBe(toggle);
      await expect.poll(() => toggle.getAnimations().length).toBe(0);
    }
  } finally { await commands.emulateReducedMotion(false); }
});
