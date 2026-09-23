// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { NativeReportView } from './public.tsx';
import { DistributionChart, linePaths, nearestSample, TimeSeriesChart } from '../../../ui/data-visualization/public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(readFileSync(resolve(process.cwd(), '../test-data/native-view-v1.json'), 'utf8')) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);

it('renders native components with no iframe or application script', () => {
  const { container } = render(<NativeReportView payload={payload} />);
  expect(container.querySelector('iframe')).toBeNull();
  expect(container.querySelector('script')).toBeNull();
  expect(screen.getByText('已用空间').parentElement?.textContent).toContain('120 GB');
  expect(screen.getByText('尚未取得样本')).toBeTruthy();
  expect(screen.getAllByRole('img')).toHaveLength(2);
  expect(container.textContent).not.toMatch(/对账|复盘|交易/);
});

it('opens evidence as text and keeps handling distinct from the finding', async () => {
  const { container } = render(<NativeReportView payload={payload} />);
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  expect(screen.getByText('待补证')).toBeTruthy();
  expect(screen.getByText('未知')).toBeTruthy();
  expect(screen.getByText('<script>alert(1)</script>')).toBeTruthy();
  expect(container.querySelector('script')).toBeNull();
});

it('opens the existing native wide dialog and restores the opener', async () => {
  render(<NativeReportView payload={payload} />);
  const button = screen.getByRole('button', { name: '展开 运营概览' });
  await userEvent.click(button);
  expect(screen.getByRole('dialog', { name: '运营概览' })).toBeTruthy();
  expect(document.querySelector('iframe')).toBeNull();
  await userEvent.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('keeps missing points as gaps and does not discard a real zero', () => {
  expect(linePaths([{ x: 0, y: 1 }, { x: 1, y: null }, { x: 2, y: 0 }])).toEqual(['M0,1', 'M2,0']);
});

it('locates dates on the actual time axis rather than evenly spaced row indices', () => {
  expect(nearestSample([0, 1, 100], 0.1)).toBe(1);
  expect(nearestSample([0, 1, 100], 0.8)).toBe(2);
  expect(nearestSample([42], 0.5)).toBe(0);
});

it('keeps isolated known observations visible on both sides of a missing sample', () => {
  const chart = payload.rows[0].cells[1];
  if (chart.kind !== 'time-series') throw new Error('Expected plot fixture');
  const { container } = render(<TimeSeriesChart label={chart.title} datasets={chart.datasets} emptyText={chart.emptyText}
    selection={{ datasetId: 'line', sample: 2, selected: null, readoutOpen: false }} onSelection={() => {}} />);
  expect(container.querySelectorAll('circle')).toHaveLength(2);
});

it('renders a single stacked observation without an invisible degenerate polygon', () => {
  const { container } = render(<TimeSeriesChart label="容量" emptyText="无数据" selection={{ datasetId: 'one', selected: null, sample: null, readoutOpen: false }} onSelection={() => {}}
    datasets={[{ id: 'one', label: '样本', unit: 'GB', style: 'stacked', series: [{ id: 'a', label: '主库', palette: 7 }], points: [{ date: '2026-09-23', values: [5] }] }]} />);
  expect(Number(container.querySelector('rect')?.getAttribute('height'))).toBeGreaterThan(0);
});

it('does not round small measured values to zero in observation inspection', () => {
  render(<TimeSeriesChart label="测量" emptyText="无数据" selection={{ datasetId: 'small', selected: null, sample: null, readoutOpen: true }} onSelection={() => {}}
    datasets={[{ id: 'small', label: '样本', unit: 'GB', style: 'line', series: [{ id: 'a', label: '主库', palette: 1 }], points: [{ date: '2026-09-23', values: [0.001] }] }]} />);
  expect(screen.getByRole('slider').getAttribute('aria-valuetext')).toContain('0.001 GB');
  expect(screen.getByText('0.001')).toBeTruthy();
});

it('preserves inspection state in both directions across wide reading', async () => {
  render(<NativeReportView payload={payload} />);
  await userEvent.click(screen.getByRole('button', { name: '合计' }));
  fireEvent.change(screen.getByRole('slider'), { target: { value: '0' } });
  await userEvent.click(screen.getByRole('button', { name: '历史用量 观察值' }));
  await userEvent.click(screen.getByRole('button', { name: '队列' }));
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  await userEvent.click(screen.getByRole('button', { name: /e1 ·/ }));
  await userEvent.click(screen.getByRole('button', { name: '展开 运营概览' }));
  const dialog = within(screen.getByRole('dialog'));
  expect(dialog.getByRole('button', { name: '合计' }).getAttribute('aria-pressed')).toBe('true');
  expect(dialog.getByRole<HTMLInputElement>('slider').value).toBe('0');
  expect(dialog.getByRole('button', { name: '历史用量 观察值' }).getAttribute('aria-expanded')).toBe('true');
  expect(dialog.getByRole('button', { name: '队列' }).getAttribute('aria-pressed')).toBe('true');
  expect(dialog.getByRole('button', { name: /e1 ·/ }).getAttribute('aria-expanded')).toBe('true');
  await userEvent.click(dialog.getByRole('button', { name: '独立序列' }));
  await userEvent.keyboard('{Escape}');
  expect(screen.getByRole('button', { name: '独立序列' }).getAttribute('aria-pressed')).toBe('true');
  expect(screen.getByRole('button', { name: '历史用量 观察值' }).getAttribute('aria-expanded')).toBe('true');
  expect(screen.getByRole('button', { name: /e1 ·/ }).getAttribute('aria-expanded')).toBe('true');
});

it('keeps complete analytical fields in details without filling the preview card', async () => {
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected record fixture');
  records.datasets[0].items[0].facts = [
    { label: 'Priority', value: 'High' }, { label: 'Next check', value: '2026-09-24' },
    { label: 'Complete source field', value: 'Retained for analysis' },
  ];
  render(<NativeReportView payload={view} />);
  expect(screen.queryByText('Retained for analysis')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  expect(screen.getByText('Retained for analysis')).toBeTruthy();
  expect(records.datasets[0].items[0].facts).toHaveLength(3);
});

it('keeps evidence control and panel IDs distinct for author-chosen suffixes', async () => {
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected record fixture');
  const record = records.datasets[0].items[0];
  record.evidence.push({ ...record.evidence[0], id: 'e1-label', body: 'Second observation' });
  const { container } = render(<NativeReportView payload={view} />);
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  const button = screen.getByRole('button', { name: /e1-label ·/ });
  const panel = document.getElementById(button.getAttribute('aria-controls')!);
  expect(panel?.getAttribute('role')).toBe('region');
  expect(panel?.getAttribute('aria-labelledby')).toBe(button.id);
  const ids = [...container.querySelectorAll('[id]')].map(element => element.id);
  expect(new Set(ids).size).toBe(ids.length);
  await userEvent.click(button);
  expect(panel?.hidden).toBe(false);
});

it('preserves the selected research scenario and evidence rather than reverting to r1', async () => {
  const example = nativeViewPayloadSchema.parse(JSON.parse(readFileSync(resolve(process.cwd(), '../plugins/paper-trading/examples/native-demo.json'), 'utf8')));
  render(<NativeReportView payload={example} />);
  await userEvent.click(screen.getByRole('button', { name: 'r2 · 预设反证' }));
  const article = screen.getByText('支持减弱').closest('article')!;
  await userEvent.click(within(article).getByRole('button', { name: '查看详情' }));
  await userEvent.click(screen.getByRole('button', { name: /E04 ·/ }));
  await userEvent.click(screen.getByRole('button', { name: '展开 低频投资组合' }));
  const dialog = within(screen.getByRole('dialog'));
  expect(dialog.getByRole('button', { name: 'r2 · 预设反证' }).getAttribute('aria-pressed')).toBe('true');
  expect(dialog.getByText('支持减弱')).toBeTruthy();
  expect(dialog.getByRole('button', { name: /E04 ·/ }).getAttribute('aria-expanded')).toBe('true');
  await userEvent.keyboard('{Escape}');
  expect(screen.getByRole('button', { name: /E04 ·/ }).getAttribute('aria-expanded')).toBe('true');
});

it('shows measured zero categories without inventing percentages or missing data', () => {
  render(<DistributionChart label="计数" unit="个" slices={[{ id: 'a', label: '已完成', value: 0, palette: 1 }]} emptyText="未取得数据" selected={null} onSelect={() => {}} />);
  expect(screen.queryByText('未取得数据')).toBeNull();
  expect(screen.getByRole('button', { name: /已完成/ }).textContent).toContain('—');
  expect(screen.getByText('计数 · 0 个')).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/NaN|100%/);
});
