// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import { ReportTableBlock } from '../table/public.tsx';

afterEach(cleanup);
const source = { source: 'neige://plugin/example/overview' };
const overview = {
  version: 1, view: 'overview', asOf: '2026-09-22T08:00:00+00:00',
  metrics: [{ label: '账户权益', value: '$100,050', detail: '券商账户总额', tone: 'neutral' }],
  notices: [{ title: '待确认调整', detail: '当前已确认策略继续生效', tone: 'warning' }],
  charts: [
    { kind: 'bars', title: '交易收益', unit: 'USD', emptyText: '暂无成交', points: [{ label: '盈利交易', value: 50 }, { label: '亏损交易', value: -20 }] },
    { kind: 'budget', title: '预算使用', unit: 'USD', detail: '成本，不是市值', used: 150, limit: 100 },
  ],
};

it('renders a validated overview instead of a table without adding write controls', () => {
  render(<ReportTableBlock payload={source} resolveLive={() => overview} />);
  expect(screen.getByText('$100,050')).toBeTruthy();
  expect(screen.getByRole('note').textContent).toContain('待确认调整');
  expect(screen.getByRole('img', { name: /交易收益.*50.*-20/ })).toBeTruthy();
  expect(screen.queryByRole('table')).toBeNull();
  expect(screen.queryByRole('button', { name: /批准|下单/ })).toBeNull();
});

it('caps the meter mark but retains the actual over-limit value', () => {
  render(<ReportTableBlock payload={source} resolveLive={() => overview} />);
  const meter = screen.getByRole('meter', { name: '预算使用' });
  expect(meter.getAttribute('value')).toBe('100');
  expect(meter.getAttribute('aria-valuetext')).toBe('150 / 100 USD');
  expect(screen.getByText('已超过当前预算')).toBeTruthy();
});

it('does not turn unknown budget or empty returns into a zero result', () => {
  const empty = { ...overview, charts: [
    { ...overview.charts[0], points: [] }, { ...overview.charts[1], used: null, limit: null },
  ] };
  render(<ReportTableBlock payload={source} resolveLive={() => empty} />);
  expect(screen.getByText('暂无成交')).toBeTruthy();
  expect(screen.getByText('暂无已确认的预算数据')).toBeTruthy();
  expect(screen.queryByRole('meter')).toBeNull();
});

it('keeps detail tables collapsed until the reader opens them', async () => {
  render(<ReportTableBlock payload={source} resolveLive={() => ({ version: 1, view: 'details', title: '策略参数',
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
  const { container } = render(<ReportTableBlock payload={source} resolveLive={() => ({ version: 1, view: 'activity', emptyText: '', items })} />);
  expect(screen.queryByText('成交 5')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '更多动态（1）' }));
  expect(screen.getByText('成交 5')).toBeTruthy();
  expect(container.querySelector('img')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '收起动态' }));
  expect(screen.queryByText('成交 5')).toBeNull();
});

it('renders review prose and a next step as a card, not JSON cells', () => {
  render(<ReportTableBlock payload={source} resolveLive={() => ({ version: 1, view: 'cards', emptyText: '',
    items: [{ id: 'r', title: '交易复盘', body: '根据成交记录完成复盘。', next: '等待下一期研究。', footer: '毛收益，未计费用' }] })} />);
  expect(screen.getByRole('region', { name: '交易复盘' })).toBeTruthy();
  expect(screen.getByText('下一步')).toBeTruthy();
  expect(screen.getByText('等待下一期研究。')).toBeTruthy();
  expect(screen.queryByRole('table')).toBeNull();
});

it('refuses unsupported view versions instead of guessing a layout', () => {
  render(<ReportTableBlock payload={source} resolveLive={() => ({ ...overview, version: 99 })} />);
  expect(screen.getByText(/cannot read as a table/)).toBeTruthy();
  expect(screen.queryByText('$100,050')).toBeNull();
});
