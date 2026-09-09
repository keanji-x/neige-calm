import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';

import '../../../styles/entry.css';
import { reportLayoutSchema, type ReportLayout } from '../../../../../core/domain/report-layout.ts';
import { ReportDocument } from '../document/public.tsx';
import portfolioBody from '../recipe/examples/portfolio.md?raw';

afterEach(cleanup);

function holdingsLayout(): ReportLayout {
  // Load presentation from the actual starter template, not a copy of its
  // columns. Backend recipe parsing/instantiation is covered by its route test.
  const layouts = [...portfolioBody.matchAll(/```neige-block layout\n([\s\S]*?)\n```/g)]
    .map(match => reportLayoutSchema.parse(JSON.parse(match[1])));
  const holdings = layouts.find(layout => layout.items.some(item => item.kind === 'table' && 'source' in item.data));
  if (holdings === undefined) throw new Error('Starter holdings layout missing');
  return holdings;
}

it('shows quoted precision through the starter table and preserves a saved fixed-decimal template', async () => {
  await page.viewport(1100, 700);
  const current = holdingsLayout();
  const source = { rows: [
    { asset: '510300', venue: 'SH', price: 4.637, currency: 'CNY', value: 32.46 },
    { asset: 'EXAMPLE', venue: 'US', price: 200, currency: 'USD', value: 100 },
    { asset: 'Total', currency: 'CNY', value: 132.46 },
  ] };
  const body = (payload: ReportLayout) => <ReportDocument empty={null} resolveLiveTable={() => source}
    report={{ summary: '', body: '', blocks: [{ id: 'holdings', kind: 'layout', payload }] }}/>;
  const { rerender } = render(body(current));
  const table = screen.getByRole('table');
  expect(within(table).getByRole('cell', { name: '4.637 CNY' })).toBeTruthy();
  expect(within(table).getByRole('cell', { name: '200.00 USD' })).toBeTruthy();
  expect(within(table).queryByText('4.63700000 CNY')).toBeNull();
  await page.screenshot({ path: '../../../../../test-results/portfolio-price-precision.png' });
  const legacy = structuredClone(current);
  for (const item of legacy.items) if (item.kind === 'table') {
    item.columns = item.columns.map(column => column.key === 'price'
      ? { key: column.key, label: column.label, format: 'number', digits: 2, suffixKey: column.suffixKey }
      : column);
  }
  rerender(body(legacy));
  expect(within(screen.getByRole('table')).getByRole('cell', { name: '4.64 CNY' })).toBeTruthy();
});
