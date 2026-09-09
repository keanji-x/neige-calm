import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import '../../../styles/entry.css';
import { reportLayoutSchema } from '../../../../../core/domain/report-layout.ts';
import { ReportDocument } from '../document/public.tsx';
import portfolioBody from '../recipe/examples/portfolio.md?raw';

afterEach(cleanup);

function starterReport() {
  // Exercise the actual seed configuration with native components. This only
  // extracts test inputs; server Recipe compilation is tested through its API.
  const blocks = [...portfolioBody.matchAll(/```neige-block layout\n([\s\S]*?)\n```/g)]
    .map((match, index) => ({ id: `layout-${index}`, kind: 'layout' as const,
      payload: reportLayoutSchema.parse(JSON.parse(match[1])) }));
  return { summary: '', body: '', blocks };
}

function projections(cash: number, includeSecurity: boolean, incomplete = false): Record<string, unknown> {
  // Published producer fixtures; arithmetic and FX conversion belong to the
  // real Market process tests, not a second implementation in this frontend.
  const security = { asset: '510300', venue: 'SH', qty: 7, price: 4.637, currency: 'CNY',
    value: 32.46, value_currency: 'CNY', weight: incomplete ? null : 24.5 };
  const total = incomplete ? null : includeSecurity ? 132.46 : cash;
  const cashValue = incomplete ? null : cash;
  const cashCurrency = incomplete ? 'USD' : 'CNY';
  return {
    'neige://plugin/dev-neige-market/portfolio.total_history': { rows: incomplete ? [] : [
      { at: '2026-09-09T08:00:00Z', total, currency: 'CNY' },
      { at: '2026-09-09T08:01:00Z', total, currency: 'CNY' },
    ] },
    'neige://plugin/dev-neige-market/portfolio.allocation': { rows: [
      ...(includeSecurity ? [{ id: 'security:SH:510300', label: '510300 · SH', kind: 'security', value: 32.46, currency: 'CNY' }] : []),
      { id: `cash:${cashCurrency}`, label: `现金 · ${cashCurrency}`, kind: 'cash', value: cashValue, currency: 'CNY' },
      { id: 'total', label: 'Total', kind: 'total', value: total, currency: incomplete ? null : 'CNY' },
    ] },
    'neige://plugin/dev-neige-market/portfolio.positions': { rows: includeSecurity ? [security] : [] },
    'neige://plugin/dev-neige-market/portfolio.cash': { rows: [
      { currency: cashCurrency, amount: cash, value: cashValue, value_currency: 'CNY', rate: incomplete ? null : 1,
        weight: incomplete || cash === 0 ? null : includeSecurity ? 75.5 : 100 },
    ], caption: incomplete ? '汇率暂不可用；保留原币余额。' : '' },
  };
}

it('uses the cash-aware seed to show separate cash and securities with whole-portfolio weights', async () => {
  await page.viewport(1280, 1100);
  const report = starterReport();
  const before = structuredClone(report);
  const sources = projections(100, true);
  const resolve = vi.fn((source: string) => sources[source]);
  render(<ReportDocument empty={null} report={report} resolveLiveTable={resolve}/>);
  const tables = screen.getAllByRole('table');
  expect(tables).toHaveLength(3);
  expect(within(tables[0]).getByRole('columnheader', { name: '余额' })).toBeTruthy();
  expect(within(tables[0]).getByRole('cell', { name: '100.00' })).toBeTruthy();
  expect(within(tables[0]).getByRole('cell', { name: '75.5%' })).toBeTruthy();
  expect(within(tables[1]).getByRole('cell', { name: '510300 SH' })).toBeTruthy();
  expect(within(tables[1]).getByRole('cell', { name: '4.637 CNY' })).toBeTruthy();
  expect(within(tables[1]).getByRole('cell', { name: '24.5%' })).toBeTruthy();
  expect(within(tables[1]).getAllByRole('row')).toHaveLength(2);
  expect(within(tables[1]).queryByText('现金')).toBeNull();
  expect(within(tables[2]).getByText('暂无记录。')).toBeTruthy();
  expect(await screen.findByRole('img', { name: '组合走势' }, { timeout: 5000 })).toBeTruthy();
  expect(await screen.findByRole('img', { name: '持仓权重' }, { timeout: 5000 })).toBeTruthy();
  expect(screen.getByRole('button', { name: '现金 · CNY 75.5%' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /Total/ })).toBeNull();
  expect(new Set(resolve.mock.calls.map(call => call[0]))).toEqual(new Set(Object.keys(sources)));
  expect([...portfolioBody.matchAll(/^# (.+)$/gm)].map(match => match[1])).toEqual(['组合概览', '持仓明细', '交易日志']);
  expect(report).toEqual(before);
  for (const table of tables) expect(getComputedStyle(table).backgroundColor).toBe('rgba(0, 0, 0, 0)');
  await page.screenshot({ path: '../../../../../test-results/portfolio-cash-mixed.png' });
});

it('shows recorded cash alone and preserves explicit zero without inventing securities or an allocation', async () => {
  await page.viewport(1100, 1000);
  const report = starterReport();
  const view = (sources: Record<string, unknown>) => <ReportDocument empty={null} report={report}
    resolveLiveTable={source => sources[source]}/>;
  const { rerender } = render(view(projections(100000, false)));
  expect(within(screen.getAllByRole('table')[0]).getByRole('cell', { name: '100,000.00' })).toBeTruthy();
  expect(await screen.findByRole('button', { name: '现金 · CNY 100.0%' }, { timeout: 5000 })).toBeTruthy();
  expect(within(screen.getAllByRole('table')[1]).getAllByRole('row')).toHaveLength(2);
  expect(within(screen.getAllByRole('table')[1]).getByText('暂无记录。')).toBeTruthy();
  rerender(view(projections(0, false)));
  expect(within(screen.getAllByRole('table')[0]).getByRole('cell', { name: '0.00' })).toBeTruthy();
  expect(await screen.findByRole('figure', { name: '持仓权重 · 环形图' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /100\.0%/ })).toBeNull();
  expect(within(screen.getAllByRole('table')[2]).getByText('暂无记录。')).toBeTruthy();
});

it('keeps native balances and quotes when valuation is incomplete without normalizing remaining securities', async () => {
  const sources = projections(100, true, true);
  render(<ReportDocument empty={null} report={starterReport()} resolveLiveTable={source => sources[source]}/>);
  expect(within(screen.getAllByRole('table')[0]).getByRole('cell', { name: '100.00' })).toBeTruthy();
  expect(within(screen.getAllByRole('table')[1]).getByRole('cell', { name: '4.637 CNY' })).toBeTruthy();
  expect(screen.getByText('汇率暂不可用；保留原币余额。')).toBeTruthy();
  expect(await screen.findByRole('figure', { name: '持仓权重 · 环形图' }, { timeout: 5000 })).toBeTruthy();
  expect(screen.queryByRole('img', { name: '持仓权重' })).toBeNull();
  expect(screen.queryByRole('button', { name: /%/ })).toBeNull();
  expect(screen.queryAllByRole('cell', { name: /%/ })).toHaveLength(0);
});

it('previews the new cash table without borrowing a current Track balance or changing the empty journal', async () => {
  const sources = projections(100000, false);
  const resolve = vi.fn((source: string) => sources[source]);
  render(<ReportDocument mode="preview" empty={null} report={starterReport()} resolveLiveTable={resolve}/>);
  const tables = screen.getAllByRole('table');
  expect(tables).toHaveLength(3);
  expect(within(tables[0]).getByRole('columnheader', { name: '余额' })).toBeTruthy();
  expect(within(tables[0]).getByText('等待数据来源更新。')).toBeTruthy();
  expect(within(tables[2]).getByText('暂无记录。')).toBeTruthy();
  expect(await screen.findByRole('figure', { name: '组合走势 · 折线图' }, { timeout: 5000 })).toBeTruthy();
  expect(await screen.findByRole('figure', { name: '持仓权重 · 环形图' }, { timeout: 5000 })).toBeTruthy();
  expect(resolve).not.toHaveBeenCalled();
  expect(screen.queryByText('100,000.00')).toBeNull();
});
