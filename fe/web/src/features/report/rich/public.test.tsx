// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import { ReportLiveViewBlock } from './public.tsx';
import { signedBarLayout } from './layout.ts';

afterEach(cleanup);
const source = { source: 'neige://plugin/example/overview', version: 1 as const, view: 'overview' as const };
const overview = {
  version: 1, view: 'overview', updated: { label: '最近对账', at: '2026-09-22T08:00:00+00:00' },
  metrics: [{ label: '账户权益', value: '$100,050', detail: '券商账户总额', tone: 'neutral' }],
  notices: [{ title: '待确认调整', detail: '当前已确认策略继续生效', tone: 'warning' }],
  charts: [
    { kind: 'bars', title: '交易收益', unit: 'USD', emptyText: '暂无成交', points: [{ label: '盈利交易', value: 50, tone: 'positive' }, { label: '亏损交易', value: -20, tone: 'negative' }] },
    { kind: 'meter', title: '预算使用', unit: 'USD', detail: '已超过当前预算', used: 150, limit: 100,
      usedLabel: '已使用', limitLabel: '上限', emptyText: '暂无已确认的预算数据', tone: 'negative' },
  ],
};

it('renders a validated overview instead of a table without adding write controls', () => {
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => overview} />);
  expect(screen.getByText('$100,050')).toBeTruthy();
  expect(screen.getByRole('note').textContent).toContain('待确认调整');
  expect(screen.getByRole('img', { name: /交易收益.*50.*-20/ })).toBeTruthy();
  expect(screen.queryByRole('table')).toBeNull();
  expect(screen.queryByRole('button', { name: /批准|下单/ })).toBeNull();
});

it('caps the meter mark but retains the actual over-limit value', () => {
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => overview} />);
  const meter = screen.getByRole('meter', { name: '预算使用' });
  expect(meter.getAttribute('value')).toBe('100');
  expect(meter.getAttribute('aria-valuetext')).toBe('150 / 100 USD');
  expect(screen.getByText('已超过当前预算')).toBeTruthy();
});

it('does not turn unknown budget or empty returns into a zero result', () => {
  const empty = { ...overview, charts: [
    { ...overview.charts[0], points: [] }, { ...overview.charts[1], used: null, limit: null },
  ] };
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => empty} />);
  expect(screen.getByText('暂无成交')).toBeTruthy();
  expect(screen.getByText('暂无已确认的预算数据')).toBeTruthy();
  expect(screen.queryByRole('meter')).toBeNull();
});

it('keeps detail tables collapsed until the reader opens them', async () => {
  render(<ReportLiveViewBlock payload={{ ...source, view: 'details' }} resolveOverlay={() => ({ version: 1, view: 'details', title: '策略参数',
    table: { columns: [{ key: 'limit', label: '上限' }], rows: [{ limit: 100 }] } })} />);
  const summary = screen.getByText('策略参数');
  expect(summary.closest('details')?.open).toBe(false);
  await userEvent.click(summary);
  expect(summary.closest('details')?.open).toBe(true);
  expect(screen.getByRole('table')).toBeTruthy();
});

it('expands readable events without rendering raw HTML', async () => {
  const items = Array.from({ length: 6 }, (_, index) => ({ id: String(index), at: '2026-09-22T08:00:00Z',
    title: `成交 ${index}`, detail: '<img src=x onerror=alert(1)>', tone: 'positive' }));
  const { container } = render(<ReportLiveViewBlock payload={{ ...source, view: 'activity' }} resolveOverlay={() => ({ version: 1, view: 'activity', emptyText: '', items })} />);
  expect(screen.queryByText('成交 5')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '展开更多（1）' }));
  expect(screen.getByText('成交 5')).toBeTruthy();
  expect(container.querySelector('img')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '收起' }));
  expect(screen.queryByText('成交 5')).toBeNull();
});

it('renders review prose and a next step as a card, not JSON cells', () => {
  render(<ReportLiveViewBlock payload={{ ...source, view: 'cards' }} resolveOverlay={() => ({ version: 1, view: 'cards', emptyText: '',
    items: [{ id: 'r', title: '交易复盘', body: '根据成交记录完成复盘。', sections: [{ label: '下一步', body: '等待下一期研究。' }], footer: '毛收益，未计费用' }] })} />);
  expect(screen.getByRole('region', { name: '交易复盘' })).toBeTruthy();
  expect(screen.getByText('下一步')).toBeTruthy();
  expect(screen.getByText('等待下一期研究。')).toBeTruthy();
  expect(screen.queryByRole('table')).toBeNull();
});

it('refuses unsupported view versions instead of guessing a layout', () => {
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => ({ ...overview, version: 99 })} />);
  expect(screen.getByRole('status').textContent).toContain('does not match');
  expect(screen.queryByText('$100,050')).toBeNull();
});

it('does not switch view types when a source publishes a different envelope', () => {
  render(<ReportLiveViewBlock payload={{ ...source, view: 'cards' }} resolveOverlay={() => overview} />);
  expect(screen.getByRole('status').textContent).toContain('does not match');
  expect(screen.queryByText('$100,050')).toBeNull();
});

it('renders non-financial semantics supplied by the App, including unfavorable positive costs', () => {
  const costs = { ...overview, updated: { label: 'Sampled', at: '2026-09-22T08:00:00Z' },
    metrics: [{ label: 'Storage', value: '120 GB', detail: 'Observed capacity', tone: 'neutral' }], notices: [],
    charts: [{ kind: 'bars', title: 'Cost change', unit: 'USD', emptyText: '', points: [
      { label: 'Increase', value: 50, tone: 'negative' }, { label: 'Saving', value: -20, tone: 'positive' },
    ] }],
  };
  const { container } = render(<ReportLiveViewBlock payload={source} resolveOverlay={() => costs} />);
  expect(screen.getByText('Sampled')).toBeTruthy();
  expect(screen.getByText('+50').className).toContain('negative');
  expect(screen.getByText('-20').className).toContain('positive');
  expect(container.textContent).not.toMatch(/对账|复盘|预算|交易/);
});

it('draws signed geometry independently of semantic tones', () => {
  expect(signedBarLayout([0])).toEqual([{ start: 0, width: 0, zero: 0 }]);
  expect(signedBarLayout([50])).toEqual([{ start: 0, width: 100, zero: 0 }]);
  expect(signedBarLayout([-100, 50])).toEqual([
    { start: 0, width: 50, zero: 50 }, { start: 50, width: 25, zero: 50 },
  ]);
});
