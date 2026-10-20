import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../../styles/entry.css';
import type { CalendarListedEntry } from '../../../../core/domain/calendar.ts';
import { CalendarTasks, type CalendarTasksProps } from './public.tsx';

afterEach(cleanup);
const tick = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
async function anchorIs(source: HTMLElement, name?: string) {
  const node = screen.getByRole('dialog', name === undefined ? undefined : { name });
  const animation = node.getAnimations()[0];
  expect(animation).toBeDefined(); await animation.ready;
  animation.pause(); animation.currentTime = 0; await tick();
  const frame = node.parentElement!.getBoundingClientRect();
  const rect = source.getBoundingClientRect();
  const point = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  const m = new DOMMatrix(getComputedStyle(node).transform);
  const p = new DOMPoint(point.x - frame.left, point.y - frame.top).matrixTransform(m);
  expect(frame.left + p.x).toBeCloseTo(point.x, 3);
  expect(frame.top + p.y).toBeCloseTo(point.y + .56, 3);
  animation.play(); await expect.poll(() => node.getAnimations().length).toBe(0);
  return node;
}
const entry: CalendarListedEntry = { id: 'one', task: { title: 'Research', description: 'Compare options', schedule: { kind: 'all_day', date: '2026-10-02' } }, version: 3, cancelled: false, source_track_id: 'source', created_by: 'agent', created_at: 1, updated_at: 1, occurrences: [] };
const calendar: CalendarTasksProps = { date: '2026-10-02', timezone: 'Asia/Shanghai', month: { entries: [], loading: false, error: null }, day: { entries: [entry], loading: false, error: null }, enabled: true, pending: false, onDateChange: vi.fn(), onWindowChange: vi.fn(), onRetry: vi.fn(), onReload: vi.fn(), onSettings: vi.fn(), onOpenTrack: vi.fn(), onSave: vi.fn(() => Promise.resolve()) };
it('opens actual Calendar new and selected keyed row editors from their own stable sources', async () => {
  await page.viewport(1200, 800);
  vi.mocked(calendar.onWindowChange).mockClear();
  render(<CalendarTasks {...calendar} />);
  // The public grid-ready callback separates lazy layout preparation from the modal's initial-frame oracle.
  await expect.poll(() => vi.mocked(calendar.onWindowChange).mock.calls.length).toBeGreaterThan(0);
  await document.fonts.ready;
  const add = screen.getByRole('button', { name: 'New task' });
  await userEvent.click(add);
  await anchorIs(add.parentElement!);
  await page.getByRole('textbox', { name: 'Task title' }).fill('A draft');
  await page.screenshot({ path: 'test-results/dialog-calendar-new.png' });
  await userEvent.keyboard('{Escape}');
  const row = screen.getByRole('button', { name: /Research/ });
  const host = row.closest('li')!;
  await userEvent.click(row);
  await anchorIs(host);
  expect(screen.getByRole('textbox', { name: 'Task title' })).toHaveValue('Research');
});
