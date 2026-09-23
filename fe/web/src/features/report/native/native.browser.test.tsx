import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../../styles/entry.css';
import source from '../../../../../../test-data/native-view-v1.json?raw';
import demoSource from '../../../../../../plugins/paper-trading/examples/native-demo.json?raw';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { NativeReportView } from './public.tsx';
import { ReportDocument } from '../document/public.tsx';
import { TimeSeriesChart } from '../../../ui/data-visualization/public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(source) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);

it.each([1440, 736, 390, 320])('native composition renders without frames or overflow at %i', async width => {
  await page.viewport(width, 1000);
  const { container } = render(<main style={{ maxInlineSize: 1000, padding: 12 }}><NativeReportView payload={payload} /></main>);
  expect(container.querySelector('iframe')).toBeNull();
  expect(container.querySelectorAll('svg').length).toBeGreaterThanOrEqual(2);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.getByRole('button', { name: '合计', exact: true }).click();
  const paths = [...container.querySelectorAll('svg path')];
  expect(paths.some(path => path.getBoundingClientRect().width > 100)).toBe(true);
  await page.getByRole('button', { name: '查看证据', exact: true }).click();
  expect(container.querySelector('script')).toBeNull();
  expect(container.textContent).toContain('<script>alert(1)</script>');
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
});

it('wide inspection is a native accessible dialog, not an embedded page', async () => {
  await page.viewport(1440, 1000);
  render(<NativeReportView payload={payload} />);
  await page.getByRole('button', { name: '展开 运营概览' }).click();
  await expect.element(page.getByRole('dialog', { name: '运营概览' })).toBeVisible();
  expect(document.querySelector('iframe')).toBeNull();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
});

it.each([1440, 390, 320])('keeps Close visible for an accepted unbroken title at %i', async width => {
  await page.viewport(width, 1000);
  render(<NativeReportView payload={{ ...payload, title: 'X'.repeat(200) }} />);
  await page.getByRole('button', { name: `展开 ${'X'.repeat(200)}` }).click();
  const close = document.querySelector<HTMLElement>('[role="dialog"] button[aria-label="Close"]')!;
  const box = close.getBoundingClientRect();
  expect(box.left).toBeGreaterThanOrEqual(0);
  expect(box.right).toBeLessThanOrEqual(width);
  expect(document.querySelector('[role="dialog"]')!.scrollWidth).toBeLessThanOrEqual(width);
});

it('keeps a native composition backlink beside its block without consuming another row', async () => {
  await page.viewport(1440, 1000);
  render(<div style={{ inlineSize: 1100, ['--document-start' as string]: '100px', ['--document-measure' as string]: '600px' }}>
    <ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'native-cited', kind: 'view', payload }] }}
      backlinkCounts={new Map([['native-cited', 3]])} empty={<p>Empty</p>} />
  </div>);
  const block = document.querySelector('#native-cited')!.getBoundingClientRect();
  const note = document.querySelector('[title="3 reports cite this block"]')!.getBoundingClientRect();
  expect(note.top).toBeLessThan(block.top + 40);
  expect(note.left).toBeGreaterThanOrEqual(block.right);
});

it('uses the authored narrow-summary ratio and keeps research visible in a wide first viewport', async () => {
  await page.viewport(1440, 1000);
  const demo = nativeViewPayloadSchema.parse(JSON.parse(demoSource));
  const { container } = render(<main style={{ inlineSize: 1120, padding: 16 }}><NativeReportView payload={demo} /></main>);
  const summary = page.getByRole('region', { name: '01 · 组合表现' }).element();
  const children = summary.querySelector('h3 + div')!.children;
  expect(children[1].getBoundingClientRect().width / children[0].getBoundingClientRect().width).toBeGreaterThan(1.8);
  expect(page.getByRole('region', { name: '03 · 投资观点' }).element().getBoundingClientRect().top).toBeLessThan(850);
  const slider = page.getByRole('slider', { name: '总资产变化 观察日期' }).element() as HTMLInputElement;
  expect(getComputedStyle(slider).opacity).toBe('0');
  slider.focus();
  await userEvent.keyboard('{Home}');
  expect(slider.value).toBe('0');
  expect(slider.getAttribute('aria-valuetext')).toContain('2026-06-30');
  expect(container.querySelector('iframe')).toBeNull();
});

it('stacks record details according to their own cell width, not the whole composition', async () => {
  await page.viewport(1440, 1000);
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  view.rows = [{ id: 'narrow', title: 'Narrow records', layout: 'three', cells: [records, view.rows[0].cells[0], view.rows[1].cells[0]] }];
  render(<main style={{ inlineSize: 1000 }}><NativeReportView payload={view} /></main>);
  await page.getByRole('button', { name: '查看证据', exact: true }).click();
  const detail = page.getByRole('region', { name: '备份是否按时完成？ 详情' }).element();
  expect(detail.getBoundingClientRect().width).toBeGreaterThan(180);
  expect(detail.getBoundingClientRect().right).toBeLessThanOrEqual(detail.parentElement!.getBoundingClientRect().right);
});

it('contains long observation tooltips inside the plot without covering the legend', async () => {
  await page.viewport(1440, 1000);
  const { container } = render(<div style={{ inlineSize: 350 }}><TimeSeriesChart label="测量" emptyText="无数据"
    selection={{ datasetId: 'long', selected: null, sample: null }} onSelection={() => {}}
    datasets={[{ id: 'long', label: '长说明', unit: 'GB', style: 'line',
      series: Array.from({ length: 6 }, (_, i) => ({ id: `s${i}`, label: `Series ${i} ${'long descriptive label '.repeat(4)}`, palette: i + 1 })),
      points: [{ date: '2026-09-23', values: [1, 2, 3, 4, 5, 6] }] }]} /></div>);
  const cursor = page.getByRole('slider').element();
  await page.getByRole('slider').hover();
  const tooltip = container.querySelector('[aria-hidden="true"][style]')!;
  expect(tooltip.getBoundingClientRect().bottom).toBeLessThanOrEqual(cursor.getBoundingClientRect().bottom);
});
