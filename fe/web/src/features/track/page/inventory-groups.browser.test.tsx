import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { PanelCard } from '../../../ui/panel-card/public.tsx';
import { makeDesktopPainter, paintDesktopPanel } from './desktop-painter.tsx';
import { card, openableCardsOf } from './test-fixtures.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });
const task = (blockId: string, status: string, kind: NonNullable<ReportTaskRow['kind']>): ReportTaskRow => ({
  blockId, key: blockId, state: 'ready', declaration: null, status, statusDetail: null,
  kind, workerCardId: `card-${blockId}`, pendingReason: null,
});
/** These cases are about layout, so every worker the fixture names is drawable and each task row keeps its kind control. */
const derive = (input: { cards: readonly CardWire[]; tasks: readonly ReportTaskRow[] }) =>
  deriveTrackPageView({ ...input, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(input.cards, input.tasks) });

it('keeps full state metadata off the visible inventory row', async () => {
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
    expect(metadata.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(status.title).not.toBe('');
  }
});

it('aligns module titles, status groups and row names, with stable secondary columns', async () => {
  await page.viewport(1200, 900);
  const view = derive({
    cards: [card({ title: 'Review workspace', runtime: { worker_session_id: 'one', kind: 'terminal', status: 'running' } })],
    tasks: [task('Check layout', 'running', 'codex'), task('A longer task name', 'verifying', 'terminal'), task('Finished review', 'done', 'claude')],
  });
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ taskSummary: '12' }), view)}</PanelCard></div>);
  for (const module of document.querySelectorAll<HTMLElement>('[data-nc-module]')) {
    const heading = module.querySelector<HTMLElement>('h2')!;
    const left = heading.getBoundingClientRect().left;
    for (const label of module.querySelectorAll<HTMLElement>('summary > span:first-child')) {
      expect(label.getBoundingClientRect().left).toBeCloseTo(left, 0);
    }
    for (const title of module.querySelectorAll<HTMLElement>('details[open] [data-nc-field="title"]')) {
      expect(title.getBoundingClientRect().left).toBeCloseTo(left, 0);
    }
  }
  const tasks = document.querySelector('[data-nc-module="tasks"]')!;
  const total = tasks.querySelector<HTMLElement>('h2 + span')!;
  expect(total.getBoundingClientRect().right).toBeCloseTo(tasks.getBoundingClientRect().right - 12, 0);
  // Group counts reserve the 20px disclosure lane; the header total uses the outer edge.
  for (const count of document.querySelectorAll<HTMLElement>('summary > span:nth-child(2)')) {
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


it.each([false, true])('right-aligns only type beside names with deletable=%s', async (deletable) => {
  await page.viewport(1200, 900);
  const worker = card({ id: 'worker', title: 'A longer worker title', kind: 'codex', deletable,
    runtime: { worker_session_id: 'session', kind: 'codex', status: 'running' } });
  const view = deriveTrackPageView({ cards: [worker],
    tasks: [{ ...task('work', 'running', 'codex'), workerCardId: 'worker' }],
    activity: { cards: { worker: 'working' } }, openableCards: new Set(['worker']) });
  const onDeleteCard = vi.fn();
  const onOpenCard = vi.fn();
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onDeleteCard, onOpenCard }), view)}</PanelCard></div>);
  const cardRow = document.querySelector<HTMLElement>('[data-nc-row="worker"]')!;
  const taskRow = document.querySelector<HTMLElement>('[data-nc-row="work"]')!;
  const cardKind = cardRow.querySelector<HTMLElement>('[data-nc-field="kind"]')!;
  const taskKind = taskRow.querySelector<HTMLElement>('[data-nc-field="kind"]')!;
  expect(cardKind.getBoundingClientRect().right).toBeCloseTo(taskKind.getBoundingClientRect().right, 0);
  expect(taskRow.getBoundingClientRect().right - taskKind.getBoundingClientRect().right).toBeCloseTo(4, 0);
  for (const kind of [cardKind, taskKind]) {
    const range = document.createRange(); range.selectNodeContents(kind);
    const style = getComputedStyle(kind);
    expect(range.getBoundingClientRect().right).toBeCloseTo(kind.getBoundingClientRect().right
      - parseFloat(style.paddingRight) - parseFloat(style.borderRightWidth), 0);
  }
  for (const row of [cardRow, taskRow]) {
    expect(row.querySelector<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(getComputedStyle(row.querySelector('[data-nc-activity]')!).animationName).toBe('none');
  }
  if (deletable) {
    await userEvent.hover(cardRow);
    const remove = cardRow.querySelector<HTMLElement>('[data-nc-row-action="delete-card"]')!;
    expect(remove.getBoundingClientRect().right).toBeLessThanOrEqual(cardKind.getBoundingClientRect().left);
    await userEvent.click(remove);
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
    expect(label.closest<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
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
