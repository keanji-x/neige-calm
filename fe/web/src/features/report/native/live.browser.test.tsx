import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../../styles/entry.css';
import source from '../../../../../../test-data/native-view-v1.json?raw';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { ReportDocument } from '../document/public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(source) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);

it.each([1440, 390, 320])('keeps long live review disclosures intact without overflow at %i', async width => {
  await page.viewport(width, 1000);
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected records fixture');
  records.datasets[0].items[0].disclosures = [{ id: 'review', label: 'Full review', body: '字'.repeat(8000), note: 'Publisher note', tone: 'neutral' }];
  render(<main style={{ maxInlineSize: 1000, padding: 12 }}><ReportDocument
    report={{ summary: '', body: '', blocks: [{ id: 'live-records', kind: 'view.live', payload: { source: 'neige://plugin/museum/collection', version: 1 } }] }}
    empty={null} resolveOverlay={() => view} /></main>);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  await page.getByRole('button', { name: 'Full review', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Full review', exact: true }).element();
  expect(panel.querySelector('blockquote')?.textContent).toBe('字'.repeat(8000));
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.getByRole('button', { name: '展开 运营概览' }).click();
  const dialog = page.getByRole('dialog');
  await expect.element(dialog.getByRole('region', { name: 'Full review', exact: true })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('button', { name: '展开 运营概览' })).toHaveFocus();
});

it('returns focus to the selected live record after closing its detail', async () => {
  await page.viewport(1440, 1000);
  render(<ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'live', kind: 'view.live',
    payload: { source: 'neige://plugin/museum/collection', version: 1 } }] }} empty={null} resolveOverlay={() => payload} />);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  await page.getByRole('button', { name: '收起详情', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '查看详情', exact: true })).toHaveFocus();
});
