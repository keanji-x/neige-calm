import { render } from '@testing-library/react';
import { page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackPage } from './public.tsx';

afterEach(() => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'working', cwd: '/tmp/alpha',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
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
        track={track}
        cards={[]}
        tasks={tasks}
        onOpenCard={onOpenCard}
        onOpenTask={onOpenTask}
        canResumeTrack={false}
        onRenameTrack={vi.fn()}
        onResumeTrack={vi.fn()}
        onDeleteTrack={vi.fn()}
      />
    </div>,
  );
  return document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
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
  it('puts the bare status before the worker kind and paints no trailing status icon', async () => {
    await browserPage.viewport(1200, 800);
    const row = renderTasks([assigned]);
    const status = row.querySelector<HTMLElement>('[data-nc-task-status-text]')!;
    const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
    const rowBox = row.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();
    const kindBox = kind.getBoundingClientRect();

    expect(rowBox.width).toBeGreaterThan(100);
    expect(status.innerText).toBe('running');
    expect(status.getAttribute('data-nc-status')).toBe('running');
    expect(statusBox.width).toBeGreaterThan(0);
    expect(statusBox.right).toBeLessThanOrEqual(kindBox.left);
    expect(row.querySelector('[role="img"][data-nc-status]')).toBeNull();
    expect(rowBox.right - kindBox.right).toBeLessThan(12);
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
    const box = status.getBoundingClientRect();

    expect(status.innerText).toBe('pending');
    expect(row.innerText).not.toContain(message);
    expect(status.title).toBe(`pending — ${message}`);
    expect(tooltipAt(box.left + box.width / 2, box.top + box.height / 2))
      .toBe(`pending — ${message}`);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    expect(reveal.title).toBe(message);
    expect(reveal.getAttribute('aria-description')).toBe(`pending — ${message}`);
  });

  it('gives the key and status to reveal, and only the worker kind to the card', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    const row = renderTasks([assigned], onOpenCard, onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const status = row.querySelector<HTMLElement>('[data-nc-task-status-text]')!;
    const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
    const statusBox = status.getBoundingClientRect();
    const kindBox = kind.getBoundingClientRect();

    expect(controlAt(statusBox.left + statusBox.width / 2, statusBox.top + statusBox.height / 2)).toBe(reveal);
    expect(controlAt(kindBox.left + kindBox.width / 2, kindBox.top + kindBox.height / 2)).toBe(kind);
    await userEvent.click(status);
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
    await userEvent.click(kind);
    expect(onOpenCard).toHaveBeenCalledWith('c-4');
    expect(onOpenTask).toHaveBeenCalledTimes(1);
  });

  it('gives a non-clickable kind label to the row reveal action', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    const row = renderTasks([{ ...assigned, kind: 'codex', workerCardId: null }], onOpenCard, onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const label = [...row.querySelectorAll<HTMLElement>('span')]
      .find((span) => span.textContent === 'codex')!;
    const box = label.getBoundingClientRect();

    expect(row.querySelector('button[title^="Open the worker card"]')).toBeNull();
    expect(controlAt(box.left + box.width / 2, box.top + box.height / 2)).toBe(reveal);
    (controlAt(box.left + box.width / 2, box.top + box.height / 2) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  it('leaves no dead trailing lane after removing the icon', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenTask = vi.fn();
    const row = renderTasks([{ ...assigned, status: null, declaration: 'Not ready' }], vi.fn(), onOpenTask);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const rowBox = row.getBoundingClientRect();
    const middle = rowBox.top + rowBox.height / 2;

    expect(row.querySelector('[data-nc-status]')).toBeNull();
    expect(controlAt(rowBox.right - 2, middle)).toBe(reveal);
    (controlAt(rowBox.right - 2, middle) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
  });

  it('highlights the row for keyboard focus and not for a pointer click', async () => {
    await browserPage.viewport(1200, 800);
    const row = renderTasks([assigned]);
    const reveal = row.querySelector<HTMLElement>('button[data-nc-row-action="reveal-block"]')!;
    const background = () => getComputedStyle(row).backgroundColor;
    const resting = background();

    await userEvent.click(reveal);
    await userEvent.unhover(row);
    expect(row.matches(':focus-within')).toBe(true);
    expect(background()).toBe(resting);

    (document.activeElement as HTMLElement | null)?.blur();
    for (let index = 0; index < 40 && document.activeElement !== reveal; index += 1) await userEvent.tab();
    expect(document.activeElement).toBe(reveal);
    expect(background()).not.toBe(resting);
  });

  it('renders every runtime status as text with its complete hover phrase', async () => {
    await browserPage.viewport(1200, 800);
    renderTasks([
      { ...assigned, blockId: 'b-running', key: 'running-task' },
      { ...assigned, blockId: 'b-pending', key: 'pending-task', status: 'pending', statusDetail: 'waiting for input' },
      { ...assigned, blockId: 'b-done', key: 'done-task', status: 'done' },
      { ...assigned, blockId: 'b-failed', key: 'failed-task', status: 'failed', statusDetail: 'command exited 1' },
    ]);
    const statuses = [...document.querySelectorAll<HTMLElement>('[data-nc-task-status-text]')];
    expect(statuses.map((status) => status.innerText)).toEqual(['running', 'pending', 'done', 'failed']);
    expect(statuses.map((status) => status.title)).toEqual([
      'running', 'pending — waiting for input', 'done', 'failed — command exited 1',
    ]);
    expect(document.querySelectorAll('[role="img"][data-nc-status]')).toHaveLength(0);
  });
});
