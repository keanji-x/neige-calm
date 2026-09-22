import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { reportLiveViewSchema } from '../../../../../core/domain/report-live-view.ts';
import { ReportTableBlock } from '../table/public.tsx';

afterEach(cleanup);

it.each([390, 1440])('keeps metrics and chart marks within a %i pixel viewport', async (width) => {
  await page.viewport(width, 1000);
  const payload = {
    version: 1, view: 'overview', asOf: '2026-09-22T08:00:00Z', notices: [],
    metrics: [{ label: '账户权益', value: '$1,000,000,000.00', detail: '券商账户总额', tone: 'neutral' },
      { label: '已实现毛收益', value: '-$2,000.00', detail: '未计费用', tone: 'negative' }],
    charts: [{ kind: 'bars', title: '已实现收益', unit: 'USD', emptyText: '',
      points: [{ label: '盈利交易', value: 50 }, { label: '亏损交易', value: -20 }] },
    { kind: 'budget', title: '预算', unit: 'USD', detail: '成本口径', used: 150, limit: 100 }],
  };
  const { container } = render(<main style={{ maxInlineSize: 600, padding: 12 }}>
    <ReportTableBlock payload={{ source: 'neige://plugin/demo/overview' }} resolveLive={() => payload} />
  </main>);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  for (const value of container.querySelectorAll('dd')) {
    const box = value.getBoundingClientRect();
    const parent = value.parentElement!.getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(parent.left);
    expect(box.right).toBeLessThanOrEqual(parent.right + 1);
  }
  const meter = container.querySelector('meter')!;
  expect(meter.getBoundingClientRect().height).toBeGreaterThan(5);
  expect(meter.value).toBe(100);
  const bars = container.querySelector('[role="img"]')!;
  expect(bars.getBoundingClientRect().width).toBeGreaterThan(150);
  expect(bars.textContent).toContain('-20');
});

it('opens reference details using the keyboard without adding approval controls', async () => {
  await page.viewport(390, 844);
  const { container } = render(<ReportTableBlock payload={{ source: 'neige://plugin/demo/detail' }} resolveLive={() => ({
    version: 1, view: 'details', title: '策略参数',
    table: { columns: [{ key: 'amount', label: '预算' }], rows: [{ amount: 1000 }] },
  })} />);
  const details = container.querySelector('details')!;
  container.querySelector('summary')!.focus();
  expect(details.open).toBe(false);
  await userEvent.keyboard('{Enter}');
  expect(details.open).toBe(true);
  expect(container.querySelector('table')!.getBoundingClientRect().height).toBeGreaterThan(10);
  expect(container.querySelector('button')).toBeNull();
});

it.each(['metric-label', 'metric-detail', 'activity-title', 'card-title', 'notice-title', 'chart-title'])(
  'contains schema-valid unbroken %s on mobile', async (field) => {
    await page.viewport(390, 844);
    const overview = { version: 1, view: 'overview', asOf: null,
      metrics: [{ label: field === 'metric-label' ? 'M'.repeat(120) : 'Equity', value: '$100',
        detail: field === 'metric-detail' ? 'D'.repeat(500) : 'Account total', tone: 'neutral' }],
      notices: field === 'notice-title' ? [{ title: 'N'.repeat(200), detail: 'Notice', tone: 'warning' }] : [],
      charts: field === 'chart-title' ? [{ kind: 'bars', title: 'C'.repeat(200), unit: 'USD', emptyText: '', points: [{ label: 'Trade', value: 10 }] }] : [],
    };
    const payload = field === 'activity-title'
      ? { version: 1, view: 'activity', emptyText: '', items: [{ id: 'event', at: '2026-09-22T08:00:00Z', title: 'A'.repeat(200), detail: 'Event detail', tone: 'neutral' }] }
      : field === 'card-title'
        ? { version: 1, view: 'cards', emptyText: '', items: [{ id: 'review', title: 'R'.repeat(200), body: 'Review body', next: '', footer: '' }] }
        : overview;
    expect(reportLiveViewSchema.safeParse(payload).success).toBe(true);
    render(<main style={{ maxInlineSize: 600, padding: 12 }}>
      <ReportTableBlock payload={{ source: 'neige://plugin/demo/long' }} resolveLive={() => payload} />
    </main>);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
  },
);
