import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { PanelCard } from '../../../ui/panel-card/public.tsx';
import { makeDesktopPainter, paintDesktopPanel } from './desktop-painter.tsx';
import { manualCard as card, openableCardsOf, renderPage } from './test-fixtures.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });
const task = (blockId: string, status: string, kind: NonNullable<ReportTaskRow['kind']>): ReportTaskRow => ({
  blockId, key: blockId, state: 'ready', declaration: null, status, statusDetail: null,
  kind, workerCardId: `card-${blockId}`, pendingReason: null,
});
/** These cases are about layout, so every worker the fixture names is drawable and each task row keeps its kind control. */
const derive = (input: { cards: readonly CardWire[]; tasks: readonly ReportTaskRow[] }) =>
  deriveTrackPageView({ ...input, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(input.cards, input.tasks) });

it('lists tool names and their real statuses without type subgroups', async () => {
  await page.viewport(1200, 900);
  const onOpenCard = vi.fn();
  const view = derive({ cards: [
    card({ id: 'running-terminal', title: 'Preview', runtime: { worker_session_id: 'one', kind: 'terminal', status: 'running' } }),
    card({ id: 'exited-terminal', title: 'Tests', runtime: { worker_session_id: 'two', kind: 'terminal', status: 'exited' } }),
    card({ id: 'exited-agent', kind: 'codex', title: 'Review', runtime: { worker_session_id: 'three', kind: 'codex', status: 'exited' } }),
  ], tasks: [] });
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenCard }), view)}</PanelCard></div>);
  expect(document.querySelectorAll('[data-nc-module="cards"] details')).toHaveLength(0);
  const statuses = [...document.querySelectorAll<HTMLElement>('[data-nc-module="cards"] [data-nc-status]')];
  expect(statuses.map(status => status.textContent)).toEqual(['running', 'exited', 'exited']);
  for (const status of statuses) expect(status.getBoundingClientRect().width).toBeGreaterThan(1);
  await userEvent.click(document.querySelector<HTMLButtonElement>('[data-nc-row="exited-terminal"] [data-nc-row-action="open-card"]')!);
  expect(onOpenCard).toHaveBeenCalledWith('exited-terminal');
});

it('keeps failed tool status visible in the expanded Workbench', async () => {
  await page.viewport(1200, 900);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}), derive({
    cards: [card({ id: 'failed-terminal', title: 'Build', runtime: { worker_session_id: 'one', kind: 'terminal', status: 'failed' } })], tasks: [],
  }))}</PanelCard></div>);
  expect(document.querySelectorAll('[data-nc-module="cards"] details')).toHaveLength(0);
  expect(document.querySelector<HTMLElement>('[data-nc-row="failed-terminal"] [data-nc-status]')?.textContent).toBe('failed');
});

it('keeps task metadata hidden while tool status remains visible', async () => {
  await page.viewport(1200, 900);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}),
    derive({ cards: [card({ id: 'pending-card', title: 'Long card title', kind: 'codex',
      runtime: { worker_session_id: 'one', kind: 'codex', status: 'running' } })],
      tasks: [{ ...task('Long task title', 'awaiting_projection', 'codex'), workerCardId: 'pending-card' }] }))}</PanelCard></div>);
  for (const summary of document.querySelectorAll<HTMLElement>('details:not([open]) > summary')) summary.click();
  const statuses = document.querySelectorAll<HTMLElement>('[data-nc-status]');
  expect(statuses).toHaveLength(2);
  for (const status of statuses) {
    const metadata = status.closest<HTMLElement>('[data-nc-inventory-metadata]')!;
    if (metadata === null) expect(status.getBoundingClientRect().width).toBeGreaterThan(1);
    else expect(metadata.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(status.title).not.toBe('');
  }
});

it('aligns text-only headings with row icons and preserves secondary columns', async () => {
  await page.viewport(1200, 900);
  const view = derive({
    cards: [card({ title: 'Review workspace', runtime: { worker_session_id: 'one', kind: 'terminal', status: 'running' } })],
    tasks: [task('Check layout', 'running', 'codex'), task('A longer task name', 'verifying', 'terminal'), task('Finished review', 'done', 'claude')],
  });
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ taskSummary: '12' }), view)}</PanelCard></div>);
  for (const module of document.querySelectorAll<HTMLElement>('[data-nc-module]')) {
    const heading = module.querySelector<HTMLElement>('h2')!;
    expect(heading.querySelector('svg')).toBeNull();
    const range = document.createRange(); range.selectNodeContents(heading.lastChild!);
    const left = range.getBoundingClientRect().left;
    for (const label of module.querySelectorAll<HTMLElement>('summary > span:first-child')) {
      expect(label.querySelector('svg')).toBeNull();
      const labelRange = document.createRange(); labelRange.selectNodeContents(label.lastChild!);
      expect(labelRange.getBoundingClientRect().left).toBeCloseTo(left, 0);
    }
    for (const title of module.querySelectorAll<HTMLElement>('details[data-nc-inventory-group][open] [data-nc-field="title"]')) {
      const icon = title.closest('li')!.querySelector('[data-nc-field="kind"] svg')!;
      expect(icon.getBoundingClientRect().left).toBeCloseTo(left, 0);
      expect(title.getBoundingClientRect().left - icon.getBoundingClientRect().right).toBeCloseTo(8, 0);
    }
  }
  const tasks = document.querySelector('[data-nc-module="tasks"]')!;
  const total = tasks.querySelector<HTMLElement>('h2 + span')!;
  expect(total.getBoundingClientRect().right).toBeCloseTo(tasks.getBoundingClientRect().right - 12, 0);
  // Group counts reserve the 20px disclosure lane; the header total uses the outer edge.
  for (const count of document.querySelectorAll<HTMLElement>('[data-nc-inventory-group] > summary > span:nth-child(2)')) {
    expect(total.getBoundingClientRect().right - count.getBoundingClientRect().right).toBeCloseTo(20, 0);
  }
  const kinds = [...tasks.querySelectorAll<HTMLElement>('details[open] [data-nc-field="kind"]')];
  expect(kinds).toHaveLength(2);
  expect(kinds[0].getBoundingClientRect().right).toBeCloseTo(kinds[1].getBoundingClientRect().right, 0);
});

it('keeps completed work folded until requested and preserves its click destination', async () => {
  const openTask = vi.fn();
  render(<div style={{ inlineSize: 260 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenTask: openTask }),
    derive({ cards: [], tasks: [task('A long completed task name', 'done', 'codex')] }))}</PanelCard></div>);
  const marker = document.querySelector<HTMLElement>('[data-nc-inventory-group] > summary > span:last-child')!;
  expect(marker.hasAttribute('data-nc-spring-rotation')).toBe(true);
  expect(getComputedStyle(marker).transitionProperty).toBe('none');
  const row = page.getByRole('button', { name: 'A long completed task name' });
  expect(document.querySelector<HTMLElement>('[data-nc-row="A long completed task name"]')!.checkVisibility()).toBe(false);
  await page.getByText('Completed', { exact: true }).click();
  await expect.element(row).toBeVisible();
  await row.click();
  expect(openTask).toHaveBeenCalledWith('A long completed task name');
});

it('bounds a long expanded group and scrolls its rows without pushing the next module away', async () => {
  await page.viewport(1200, 800);
  const openTask = vi.fn();
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenTask: openTask }),
    derive({ cards: [], tasks: Array.from({ length: 40 }, (_, index) => task(`Task ${index + 1}`, 'running', 'codex')) }))}
    <div data-testid="next-module">Conversations</div></PanelCard></div>);
  const group = document.querySelector<HTMLElement>('[data-nc-inventory-group="working"]')!;
  const scrollport = group.querySelector<HTMLElement>('summary + div')!;
  expect(group.getBoundingClientRect().height).toBeLessThan(240);
  expect(scrollport.scrollHeight).toBeGreaterThan(scrollport.clientHeight);
  expect(document.querySelector('[data-testid="next-module"]')!.getBoundingClientRect().top).toBeLessThan(370);
  scrollport.scrollTop = scrollport.scrollHeight;
  await page.getByRole('button', { name: 'Task 40' }).click();
  expect(openTask).toHaveBeenCalledWith('Task 40');
});

it('shares the module height across several expanded groups', async () => {
  await page.viewport(1200, 800);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}),
    derive({ cards: [], tasks: ['running', 'failed', 'done'].flatMap(status =>
      Array.from({ length: 30 }, (_, index) => task(`${status} ${index}`, status, 'codex'))) }))}
    <div data-testid="next-module">Conversations</div></PanelCard></div>);
  for (const summary of document.querySelectorAll<HTMLElement>('[data-nc-module="tasks"] details:not([open]) > summary')) summary.click();
  await expect.poll(() => document.querySelector('[data-testid="next-module"]')!.getBoundingClientRect().top).toBeLessThan(440);
  const groups = document.querySelectorAll<HTMLElement>('[data-nc-module="tasks"] details[open]');
  expect(groups).toHaveLength(3);
  for (const group of groups) {
    const rows = group.querySelector<HTMLElement>('summary + div')!;
    expect(rows.clientHeight).toBeGreaterThan(20);
    expect(rows.scrollHeight).toBeGreaterThan(rows.clientHeight);
  }
});

it('keeps the heading stationary and uses 4px content spacing with an 8px group footer', async () => {
  await page.viewport(1200, 800);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}),
    derive({ cards: [], tasks: [task('Waiting task', 'pending', 'codex'), task('Completed task', 'done', 'codex')] }))}</PanelCard></div>);
  const waiting = document.querySelector<HTMLElement>('[data-nc-inventory-group="waiting"] summary')!;
  const completed = document.querySelector<HTMLElement>('[data-nc-inventory-group="done"] summary')!;
  expect(completed.getBoundingClientRect().top - waiting.getBoundingClientRect().top).toBeCloseTo(28, 0);
  const closedTop = waiting.getBoundingClientRect().top;
  await page.getByText('Waiting', { exact: true }).click();
  expect(waiting.getBoundingClientRect().top).toBeCloseTo(closedTop, 0);
  const item = document.querySelector<HTMLElement>('[data-nc-row="Waiting task"]')!;
  expect(item.getBoundingClientRect().top - waiting.getBoundingClientRect().top).toBeCloseTo(32, 0);
  const group = waiting.closest('details')!;
  const panelBody = document.querySelector<HTMLElement>('[data-nc-module="tasks"] > div:last-child')!;
  const panelInset = Number.parseFloat(getComputedStyle(panelBody).paddingInlineStart);
  expect(panelInset).toBe(12);
  expect(Number.parseFloat(getComputedStyle(group).paddingBlockStart)).toBe(0);
  expect(Number.parseFloat(getComputedStyle(group).paddingBlockEnd)).toBe(8);
  expect(completed.getBoundingClientRect().top - item.getBoundingClientRect().top).toBeCloseTo(36, 0);
  await page.getByText('Waiting', { exact: true }).click();
  expect(completed.getBoundingClientRect().top - waiting.getBoundingClientRect().top).toBeCloseTo(28, 0);
});


it.each([false, true])('left-aligns type icons beside names with deletable=%s', async (deletable) => {
  await page.viewport(1200, 900);
  const worker = card({ id: 'worker', title: 'A longer worker title', kind: 'codex', deletable,
    runtime: { worker_session_id: 'session', kind: 'codex', status: 'running' } });
  const view = deriveTrackPageView({ cards: [worker, card({ id: 'execution-worker', title: 'Execution', kind: 'codex' })],
    tasks: [{ ...task('work', 'running', 'codex'), workerCardId: 'execution-worker' }],
    activity: { cards: { worker: 'working', 'execution-worker': 'working' } }, openableCards: new Set(['worker', 'execution-worker']) });
  const onDeleteCard = vi.fn();
  const onOpenCard = vi.fn();
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onDeleteCard, onOpenCard }), view.desktop)}</PanelCard></div>);
  for (const summary of document.querySelectorAll<HTMLElement>('[data-nc-module="work"] details:not([open]) > summary')) await userEvent.click(summary);
  const cardRow = document.querySelector<HTMLElement>('[data-nc-row="tool:worker"]')!;
  const taskRow = document.querySelector<HTMLElement>('[data-nc-row="task:work"]')!;
  const cardKind = cardRow.querySelector<HTMLElement>('[data-nc-field="kind"]')!;
  const taskKind = taskRow.querySelector<HTMLElement>('[data-nc-field="kind"]')!;
  const cardIcon = cardRow.querySelector('svg')!.getBoundingClientRect();
  expect(cardKind.textContent).toBe('codex');
  expect(cardKind.getBoundingClientRect().width).toBeLessThanOrEqual(1);
  expect(taskKind.getBoundingClientRect().width).toBeLessThanOrEqual(1);
  const taskIcon = taskRow.querySelector('svg')!.getBoundingClientRect();
  expect(cardIcon.left).toBeCloseTo(taskIcon.left, 0);
  for (const row of [cardRow, taskRow]) {
    const icon = row.querySelector('svg')!.getBoundingClientRect();
    const name = row.querySelector<HTMLElement>('[data-nc-field="title"]')!.getBoundingClientRect();
    expect(icon.width).toBe(14);
    expect(name.left - icon.right).toBeCloseTo(8, 0);
  }
  for (const row of [cardRow, taskRow]) {
    expect(row.querySelector<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(getComputedStyle(row.querySelector('[data-nc-activity]')!).animationName).toBe('none');
  }
  if (deletable) {
    await userEvent.hover(cardRow);
    const menu = cardRow.querySelector<HTMLElement>('button[aria-haspopup="menu"]')!;
    expect(menu.getBoundingClientRect().right).toBeCloseTo(cardRow.getBoundingClientRect().right - 4, 0);
    await userEvent.click(menu);
    await page.getByRole('menuitem', { name: 'Delete card A longer worker title' }).click();
    expect(onDeleteCard).toHaveBeenCalledWith('worker');
    expect(onOpenCard).not.toHaveBeenCalled();
  }
});


it('keeps inventory activity metadata nonvisual and free of animation', async () => {
  await page.viewport(1200, 900);
  const worker = card({ id: 'worker', title: 'Worker', kind: 'codex',
    runtime: { worker_session_id: 'session', kind: 'codex', status: 'running' } });
  const view = deriveTrackPageView({ cards: [worker],
    tasks: [{ ...task('work', 'running', 'codex'), workerCardId: 'worker' }],
    activity: { cards: { worker: 'working' } }, openableCards: new Set(['worker']) });
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}), view)}</PanelCard></div>);
  const running = document.querySelectorAll<HTMLElement>('[data-nc-status="running"]');
  expect(running).toHaveLength(2);
  for (const label of running) {
    expect(label.textContent).toBe('running');
    const metadata = label.closest<HTMLElement>('[data-nc-inventory-metadata]');
    if (metadata !== null) expect(metadata.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    else expect(label.getBoundingClientRect().width).toBeGreaterThan(1);
  }
  expect(document.querySelectorAll('[data-nc-activity="working"]')).toHaveLength(2);
  for (const marker of document.querySelectorAll('[data-nc-activity]')) expect(getComputedStyle(marker).animationName).toBe('none');
});

it('keeps a visible task anchored when rows above it are inserted and removed', async () => {
  await page.viewport(1200, 800);
  const tasks = Array.from({ length: 60 }, (_, index) => task(`Task ${String(index).padStart(2, '0')}`, 'running', 'codex'));
  const content = (rows: readonly ReportTaskRow[]) => <div style={{ inlineSize: 300 }}><PanelCard>
    {paintDesktopPanel(makeDesktopPainter({}), derive({ cards: [], tasks: rows }))}
  </PanelCard></div>;
  const view = render(content(tasks));
  const group = document.querySelector<HTMLElement>('[data-nc-inventory-group="working"]')!;
  const scrollport = group.querySelector<HTMLElement>('summary + div')!;
  const anchor = await page.getByRole('button', { name: 'Task 30', exact: true }).findElement();
  scrollport.scrollTop += anchor.getBoundingClientRect().top - scrollport.getBoundingClientRect().top;
  const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  await frame();
  const before = anchor.getBoundingClientRect().top;
  view.rerender(content([task('Inserted above', 'running', 'codex'), ...tasks]));
  await frame();
  expect(anchor.isConnected).toBe(true);
  expect(anchor.getBoundingClientRect().top).toBeCloseTo(before, 0);
  view.rerender(content(tasks.slice(10)));
  await frame();
  expect(anchor.isConnected).toBe(true);
  expect(anchor.getBoundingClientRect().top).toBeCloseTo(before, 0);
});


it('lists tools directly while keeping kernel attention visible', async () => {
  await page.viewport(1200, 900);
  const worker = card({ id: 'attention-worker', title: 'Needs a reply', kind: 'codex' });
  const view = deriveTrackPageView({ cards: [worker], tasks: [],
    activity: { cards: { 'attention-worker': 'input' } }, openableCards: new Set([worker.id]) });
  const onOpenCard = vi.fn();
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenCard }), view)}</PanelCard></div>);
  const inventory = document.querySelector<HTMLElement>('[data-nc-module="cards"]')!;
  expect(inventory.querySelectorAll('details')).toHaveLength(0);
  expect(inventory.textContent).toContain('Needs input');
  await page.getByRole('button', { name: 'Needs a reply' }).click();
  expect(onOpenCard).toHaveBeenCalledWith(worker.id);
});


it('reveals module actions on hover and keyboard focus', async () => {
  await page.viewport(1200, 900);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({
    cardsAction: <button type="button" aria-label="Add a tool">+</button>,
  }), derive({ cards: [card()], tasks: [] }))}</PanelCard></div>);
  const action = document.querySelector<HTMLElement>('[aria-label="Add a tool"]')!;
  await page.elementLocator(document.body).hover();
  expect(getComputedStyle(action.parentElement!).opacity).toBe('0');
  action.focus();
  expect(getComputedStyle(action.parentElement!).opacity).toBe('1');
  action.blur();
  await page.elementLocator(action.closest('section')!).hover();
  expect(getComputedStyle(action.parentElement!).opacity).toBe('1');
});


it('bounds the flat tool list and keeps the last tool reachable', async () => {
  await page.viewport(1200, 800);
  const onOpenCard = vi.fn();
  const cards = Array.from({ length: 40 }, (_, index) => card({ id: `tool-${index}`, title: `Tool ${index + 1}` }));
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenCard }),
    derive({ cards, tasks: [] }))}<div data-testid="after-tools">Conversations</div></PanelCard></div>);
  const list = document.querySelector<HTMLElement>('[data-nc-module="cards"] ul')!;
  expect(list.scrollHeight).toBeGreaterThan(list.clientHeight);
  expect(list.clientHeight).toBeLessThan(300);
  expect(document.querySelector('[data-testid="after-tools"]')!.getBoundingClientRect().top).toBeLessThan(440);
  list.scrollTop = list.scrollHeight;
  await page.getByRole('button', { name: 'Tool 40' }).click();
  expect(onOpenCard).toHaveBeenCalledWith('tool-39');
});

it('groups work and conversation entries in the real page without duplicating status labels', async () => {
  await page.viewport(1400, 900);
  const openConversation = vi.fn();
  const newConversation = vi.fn();
  renderPage({
    cards: [card({ title: 'Development server' })], tasks: [task('Review layout', 'running', 'codex')],
    conversationList: <button type="button" onClick={openConversation}>Planner conversation</button>,
    cardsAction: <button type="button" onClick={newConversation}>New</button>,
  });
  const desktop = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  expect([...desktop.querySelectorAll('h2')].map(title => title.textContent)).toEqual(['Conversations', 'Progress', 'Workbench', 'Workbench']);
  const entries = desktop;
  const conversation = [...entries.querySelectorAll('button')].find(button => button.textContent === 'Planner conversation')!;
  await userEvent.click(conversation);
  expect(openConversation).toHaveBeenCalledOnce();
  const create = [...entries.querySelectorAll('button')].find(button => button.textContent === 'New')!;
  create.focus();
  expect(getComputedStyle(create.parentElement!).opacity).toBe('1');
  await userEvent.keyboard('{Enter}');
  expect(newConversation).toHaveBeenCalledOnce();
});

it.each(['input', 'failed'] as const)('keeps kernel %s visible alongside a running tool session', async (activity) => {
  await page.viewport(1200, 900);
  const worker = card({ id: 'busy-worker', title: 'Preview', runtime: {
    worker_session_id: 'busy-session', kind: 'terminal', status: 'running',
  } });
  const view = deriveTrackPageView({ cards: [worker], tasks: [],
    activity: { cards: { [worker.id]: activity } }, openableCards: new Set([worker.id]) });
  render(<PanelCard>{paintDesktopPanel(makeDesktopPainter({}), view)}</PanelCard>);
  const name = document.querySelector('[data-nc-row="busy-worker"]')!;
  const expected = activity === 'input' ? 'Needs input' : 'Needs attention';
  const label = [...name.querySelectorAll('span')].find(element => element.textContent === expected);
  expect(label, 'attention must be in the visible name area, outside clipped metadata').toBeDefined();
  expect(label!.getBoundingClientRect().width).toBeGreaterThan(1);
  expect(name.querySelector('[data-nc-status]')?.textContent).toBe('running');
});


it('collapses Workbench while retaining tools and usable header actions', async () => {
  await page.viewport(1200, 900);
  const onAdd = vi.fn();
  const onOpenCard = vi.fn();
  const content = {
    cardsAction: <button type="button" onClick={onAdd}>Add tool</button>,
    onOpenCard,
    cards: [card({ id: 'server', title: 'Development server' })],
    tasks: [task('Check layout', 'running', 'codex')],
  };
  renderPage(content);
  const group = document.querySelector<HTMLElement>('[data-nc-desktop-panel] [data-nc-module="cards"]')!;
  const disclosure = group.querySelector<HTMLButtonElement>('button[aria-labelledby]')!;
  expect(disclosure).not.toBeNull();
  expect(disclosure.getAttribute('aria-expanded')).toBe('true');
  const tool = group.querySelector<HTMLButtonElement>('[data-nc-row="tool:server"] button')!;
  await page.elementLocator(disclosure).click();
  expect(disclosure.getAttribute('aria-expanded')).toBe('false');
  expect(tool.isConnected).toBe(true);
  expect(tool.checkVisibility()).toBe(false);
  await page.getByRole('button', { name: 'Add tool', exact: true }).click();
  expect(onAdd).toHaveBeenCalledOnce();
  expect(disclosure.getAttribute('aria-expanded')).toBe('false');
  disclosure.focus();
  await userEvent.keyboard('{Enter}');
  expect(disclosure.getAttribute('aria-expanded')).toBe('true');
  expect(group.querySelector('[data-nc-row="tool:server"] button')).toBe(tool);
  await page.getByRole('button', { name: 'Development server', exact: true }).click();
  expect(onOpenCard).toHaveBeenCalledWith('server');
});
