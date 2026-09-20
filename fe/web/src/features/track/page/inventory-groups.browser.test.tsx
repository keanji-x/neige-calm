import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { PanelCard } from '../../../ui/panel-card/public.tsx';
import { makeDesktopPainter, paintDesktopPanel } from './desktop-painter.tsx';
import { card, openableCardsOf } from './test-fixtures.tsx';

afterEach(() => { document.body.replaceChildren(); });
const task = (blockId: string, status: string, kind: NonNullable<ReportTaskRow['kind']>): ReportTaskRow => ({
  blockId, key: blockId, state: 'ready', declaration: null, status, statusDetail: null,
  kind, workerCardId: `card-${blockId}`, pendingReason: null,
});
/** These cases are about layout, so every worker the fixture names is drawable and each task row keeps its kind control. */
const derive = (input: { cards: readonly CardWire[]; tasks: readonly ReportTaskRow[] }) =>
  deriveTrackPageView({ ...input, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(input.cards, input.tasks) });

it('clips long status tokens before the worker-type column while keeping their full explanation', async () => {
  await page.viewport(1200, 900);
  render(<div style={{ inlineSize: 300 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({}),
    derive({ cards: [card({ id: 'pending-card', title: 'Long card title', kind: 'codex',
      runtime: { worker_session_id: 'one', kind: 'codex', status: 'running' } })],
      tasks: [{ ...task('Long task title', 'awaiting_projection', 'codex'), workerCardId: 'pending-card' }] }))}</PanelCard></div>);
  for (const summary of document.querySelectorAll<HTMLElement>('details:not([open]) > summary')) summary.click();
  const statuses = document.querySelectorAll<HTMLElement>('[data-nc-status]');
  expect(statuses).toHaveLength(2);
  for (const status of statuses) {
    const style = getComputedStyle(status);
    expect(status.scrollWidth).toBeGreaterThan(status.clientWidth);
    expect(style.overflowX).toBe('hidden');
    expect(style.textOverflow).toBe('ellipsis');
    expect(status.title).not.toBe('');
    const kind = status.closest('[data-nc-row]')!.querySelector('[data-nc-field="kind"]')!;
    expect(status.getBoundingClientRect().right).toBeLessThanOrEqual(kind.getBoundingClientRect().left);
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
  for (const count of document.querySelectorAll<HTMLElement>('summary > span:nth-child(2)')) {
    expect(count.getBoundingClientRect().right).toBeCloseTo(total.getBoundingClientRect().right, 0);
  }
  const statuses = [...tasks.querySelectorAll<HTMLElement>('details[open] [data-nc-status]')];
  expect(statuses).toHaveLength(2);
  expect(statuses[0].getBoundingClientRect().right).toBeCloseTo(statuses[1].getBoundingClientRect().right, 0);
  const kinds = [...tasks.querySelectorAll<HTMLElement>('details[open] [data-nc-field="kind"]')];
  expect(kinds[0].getBoundingClientRect().right).toBeCloseTo(kinds[1].getBoundingClientRect().right, 0);
});

it('keeps completed work folded until requested and preserves its click destination', async () => {
  const openTask = vi.fn();
  render(<div style={{ inlineSize: 260 }}><PanelCard>{paintDesktopPanel(makeDesktopPainter({ onOpenTask: openTask }),
    derive({ cards: [], tasks: [task('A long completed task name', 'done', 'codex')] }))}</PanelCard></div>);
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
