import { cleanup, fireEvent, render } from '@testing-library/react';
import { page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackPage } from './public.tsx';

afterEach(() => {
  cleanup(); document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, cwd: '/tmp/alpha', isolation: 'none', agentCwd: '/tmp/alpha',
  pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
};

const assigned: ReportTaskRow = {
  blockId: 'b-bench', key: 'bench-harness', state: 'ready',
  declaration: null, status: 'running', statusDetail: null, kind: 'terminal', workerCardId: 'c-4', pendingReason: null,
};

function renderTasks(
  tasks: readonly ReportTaskRow[],
  onOpenCard = vi.fn(),
  onOpenTask = vi.fn(),
) {
  render(
    <div style={{ inlineSize: 1200, blockSize: 800 }}>
      <TrackPage
        mobilePanelObscured={false}
        track={track}
        cards={[]}
        tasks={tasks}
        /* `assigned`'s worker is drawable here; the case with no card sets
           `workerCardId: null` on the task itself. */
        openableCards={new Set(['c-4'])}
        onOpenCard={onOpenCard}
        onOpenTask={onOpenTask}
        canReopenTrack={false}
        canCloseTrack={false} adminGrantedAt={null}
        onRenameTrack={vi.fn()}
        onReopenTrack={vi.fn()}
        onCloseTrack={vi.fn()}
        onDeleteTrack={vi.fn()} onTrackDeleted={vi.fn()}
      />
    </div>,
  );
  // These cases inspect row hit targets after the reader discloses their group.
  for (const summary of document.querySelectorAll<HTMLElement>('[data-nc-work-inventory] button[aria-expanded="false"][aria-controls]:not([aria-haspopup])')) fireEvent.click(summary);
  return document.querySelector<HTMLElement>('[data-nc-work-inventory] li')!;
}

function controlAt(x: number, y: number): Element | null {
  return document.elementFromPoint(x, y)?.closest('button') ?? null;
}

function tooltipAt(x: number, y: number): string | null {
  let element: Element | null = document.elementFromPoint(x, y);
  while (element !== null) {
    const title = element.getAttribute('title');
    if (title) return title;
    element = element.parentElement;
  }
  return null;
}

describe('a compact desktop TASKS row', () => {
  it('shows the name, leading worker icon and trailing status on one line', async () => {
    await browserPage.viewport(1200, 800);
    const row = renderTasks([assigned]);
    const status = row.querySelector<HTMLElement>('[data-nc-task-status-text]')!;
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const rowBox = row.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();
    const revealBox = reveal.getBoundingClientRect();

    expect(rowBox.width).toBeGreaterThan(100);
    expect(status.textContent).toBe('running');
    expect(status.closest<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(status.getAttribute('data-nc-status')).toBe('running');
    expect(statusBox.width).toBeGreaterThan(0);
    expect(status.closest<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(row.querySelector('[role="img"][data-nc-status]')).toBeNull();
    expect(revealBox.left).toBeCloseTo(rowBox.left, 0);
    const icon = reveal.querySelector('svg')!.getBoundingClientRect();
    const name = row.querySelector<HTMLElement>('[data-nc-field="title"]')!.getBoundingClientRect();
    expect(icon.width).toBe(14);
    expect(name.left - icon.right).toBeCloseTo(8, 0);
  });

  it('keeps a pending reason out of the row copy and exposes it on hover', async () => {
    await browserPage.viewport(1200, 800);
    const message = 'Waiting for `status-hierarchy`';
    const row = renderTasks([{
      ...assigned,
      status: 'pending',
      kind: 'codex',
      workerCardId: null,
      pendingReason: {
        kind: 'dependencyBlocked', message, dependencies: ['status-hierarchy'],
      },
    }]);
    const status = row.querySelector<HTMLElement>('[data-nc-task-status-text]')!;
    expect(row.querySelector<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(status.title).toBe(`pending — ${message}`);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const box = reveal.getBoundingClientRect();
    expect(tooltipAt(box.left + box.width / 2, box.top + box.height / 2)).toBe(message);
    expect(reveal.title).toBe(message);
    expect(reveal.getAttribute('aria-description')).toBe(`pending — ${message}`);
  });

  it('keeps the full row as reveal and the execution destination in its menu', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    const row = renderTasks([assigned], onOpenCard, onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const box = reveal.getBoundingClientRect();
    expect(controlAt(box.left + box.width / 2, box.top + box.height / 2)).toBe(reveal);
    await userEvent.click(reveal);
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
    await userEvent.hover(row);
    await browserPage.getByRole('button', { name: 'Actions for bench-harness', exact: true }).click();
    await browserPage.getByRole('menuitem', { name: 'View execution', exact: true }).click();
    expect(onOpenCard).toHaveBeenCalledWith('c-4');
    expect(onOpenTask).toHaveBeenCalledTimes(1);
  });

  it('keeps the provider icon within the reveal action when there is no execution', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    const row = renderTasks([{ ...assigned, kind: 'codex', workerCardId: null }], onOpenCard, onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const label = reveal.querySelector<SVGElement>('svg')!;
    const box = label.getBoundingClientRect();

    expect(row.querySelector('button[title^="Open the worker card"]')).toBeNull();
    expect(controlAt(box.left + box.width / 2, box.top + box.height / 2)).toBe(reveal);
    (controlAt(box.left + box.width / 2, box.top + box.height / 2) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  it('leaves no dead trailing lane when no secondary destination exists', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenTask = vi.fn();
    const row = renderTasks([{ ...assigned, status: null, declaration: 'Not ready', workerCardId: null }], vi.fn(), onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const rowBox = row.getBoundingClientRect();
    const middle = rowBox.top + rowBox.height / 2;

    expect(row.querySelector('[data-nc-status]')).toBeNull();
    expect(controlAt(rowBox.right - 2, middle)).toBe(reveal);
    (controlAt(rowBox.right - 2, middle) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
  });

  it('draws a visible focus ring for keyboard navigation', async () => {
    await browserPage.viewport(1200, 800);
    const row = renderTasks([assigned]);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const outline = () => getComputedStyle(reveal).outlineStyle;

    await userEvent.click(reveal);
    await userEvent.unhover(row);
    expect(row.matches(':focus-within')).toBe(true);
    expect(reveal.matches(':focus-visible')).toBe(false);

    (document.activeElement as HTMLElement | null)?.blur();
    for (let index = 0; index < 40 && document.activeElement !== reveal; index += 1) await userEvent.tab();
    expect(document.activeElement).toBe(reveal);
    expect(reveal.matches(':focus-visible')).toBe(true);
    expect(outline()).toBe('solid');
    expect(getComputedStyle(reveal).outlineWidth).toBe('2px');
  });

  it('preserves full runtime descriptions alongside compact state labels', async () => {
    await browserPage.viewport(1200, 800);
    renderTasks([
      { ...assigned, blockId: 'b-running', key: 'running-task' },
      { ...assigned, blockId: 'b-pending', key: 'pending-task', status: 'pending', statusDetail: 'waiting for input' },
      { ...assigned, blockId: 'b-done', key: 'done-task', status: 'done' },
      { ...assigned, blockId: 'b-failed', key: 'failed-task', status: 'failed', statusDetail: 'command exited 1' },
    ]);
    const statuses = [...document.querySelectorAll<HTMLElement>('[data-nc-task-status-text]')];
    expect(statuses.map((status) => status.textContent)).toEqual(['running', 'pending', 'failed', 'done']);
    expect(statuses.map((status) => status.title)).toEqual([
      'running', 'pending — waiting for input', 'failed — command exited 1', 'done',
    ]);
    for (const status of statuses) expect(status.closest<HTMLElement>('[data-nc-inventory-metadata]')!.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(document.querySelectorAll('[role="img"][data-nc-status]')).toHaveLength(0);
  });
});
