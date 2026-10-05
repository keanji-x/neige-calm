import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../../styles/entry.css';
import source from '../../../../../../test-data/native-view-v1.json?raw';
import demoSource from '../../../../../../plugins/paper-trading/examples/native-demo.json?raw';
import manifestSource from '../../../../../../plugins/paper-trading/manifest.json?raw';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { trackOverlayPayload, type OverlayWire } from '../../../../../core/domain/track.ts';
import { NativeReportView } from './public.tsx';
import { ReportDocument } from '../document/public.tsx';
import { TimeSeriesChart } from '../../../ui/data-visualization/public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(source) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);
/** The real App example: the recipe's template views over the App's units, through the production lookup. */
const demo = JSON.parse(demoSource) as { views: unknown[]; overlays: Record<string, unknown> };
const demoPlugin = (JSON.parse(manifestSource) as { id: string }).id;
const demoOverlays: OverlayWire[] = Object.entries(demo.overlays).map(([kind, unit]) => ({
  id: kind, plugin_id: demoPlugin, entity_kind: 'track', entity_id: 'example', kind, payload: unit, updated_at: 0,
}));
const resolveDemo = (source: string) => trackOverlayPayload('example', demoOverlays, source);
const performance = nativeViewPayloadSchema.parse(demo.views[0]);

function checkDonut(svg: Element) {
  const texts = [...svg.querySelectorAll<SVGTextElement>('text')];
  for (const text of texts) {
    expect(parseFloat(getComputedStyle(text).fontSize)).toBeGreaterThanOrEqual(12);
    const box = text.getBBox();
    expect(box.width).toBeGreaterThan(0);
    for (const x of [box.x, box.x + box.width]) {
      for (const y of [box.y, box.y + box.height]) expect(Math.hypot(x - 75, y - 75)).toBeLessThan(41);
    }
  }
  expect(texts[0].getBoundingClientRect().bottom).toBeLessThan(texts[1].getBoundingClientRect().top);
}

it.each([1440, 320])('keeps distribution totals and selected shares inside the donut at %i', async width => {
  await page.viewport(width, 1000);
  const view = structuredClone(payload);
  const chart = view.rows[1].cells[0];
  if (chart.kind !== 'distribution') throw new Error('Expected distribution fixture');
  chart.unit = '每秒处理的完整数据记录';
  chart.slices[0].value = 123456789.12345678;
  chart.slices[1].value = 0;
  const { container, rerender } = render(<main style={{ maxInlineSize: 1000, padding: 12 }}><NativeReportView payload={view} /></main>);
  const svg = () => page.getByRole('img', { name: /^存储构成:/ }).element();
  const center = () => [...svg().querySelectorAll('text')].map(text => text.textContent);
  const checkLayout = () => {
    checkDonut(svg());
    expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(width);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  };
  expect(center()).toEqual(['123.46M', '总量']);
  expect(container.textContent).toContain(`123,456,789.12345678 ${chart.unit}`);
  checkLayout();
  await page.screenshot({ path: `__screenshots__/distribution-total-${width}.png` });
  await page.getByRole('button', { name: /备份\s*0%/ }).click();
  expect(center()).toEqual(['0%', '占比']);
  checkLayout();
  await page.getByRole('button', { name: /备份\s*0%/ }).click();
  expect(center()).toEqual(['123.46M', '总量']);
  checkLayout();
  await page.getByRole('button', { name: /主库\s*100%/ }).click();
  expect(center()).toEqual(['100%', '占比']);
  checkLayout();
  await page.screenshot({ path: `__screenshots__/distribution-selected-${width}.png` });
  chart.slices = [{ id: 'replacement', label: '新样本', value: 0.001, palette: 3 }];
  rerender(<main style={{ maxInlineSize: 1000, padding: 12 }}><NativeReportView payload={view} /></main>);
  expect(center()).toEqual(['0.001', '总量']);
  checkLayout();
});

it.each([1440, 320])('keeps boundary totals readable inline and in inspection at %i', async width => {
  await page.viewport(width, 1000);
  const view = structuredClone(payload);
  const chart = view.rows[1].cells[0];
  if (chart.kind !== 'distribution') throw new Error('Expected distribution fixture');
  view.rows = [{ id: 'totals', title: '', layout: 'one', cells: [chart] }];
  const { container, rerender } = render(<main style={{ maxInlineSize: 700, padding: 12 }}><NativeReportView payload={view} /></main>);
  for (const [value, unit, count, center] of [
    [123456.78, 'USD', 1, '123.46K'],
    [1e15, '字'.repeat(32), 12, '12,000T'],
    [999999.99, 'W'.repeat(32), 1, '1M'],
    [12.3456, 'USD', 1, '12.3456'],
    [1e-300, '字'.repeat(32), 1, '1e-300'],
    [1.23456789e-300, '字'.repeat(32), 1, '1.2e-300'],
    [999.9999, 'USD', 1, '999.9999'],
    [0, '字'.repeat(32), 1, '0'],
  ] as const) {
    chart.unit = unit;
    chart.slices = Array.from({ length: count }, (_, i) => ({ id: `s${i}`, label: `样本 ${i}`, value, palette: 1 }));
    const accepted = nativeViewPayloadSchema.parse(view);
    rerender(<main style={{ maxInlineSize: 700, padding: 12 }}><NativeReportView payload={accepted} /></main>);
    const svg = page.getByRole('img', { name: /^存储构成:/ }).element();
    expect([...svg.querySelectorAll('text')].map(text => text.textContent)).toEqual([center, '总量']);
    expect(container.textContent).toContain(unit);
    if (value === 12.3456) expect(container.textContent).toContain('12.3456 USD');
    if (count === 12) expect(container.textContent).toContain('12,000,000,000,000,000');
    checkDonut(svg);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    if (value === 123456.78) await page.screenshot({ path: `__screenshots__/distribution-usd-${width}.png` });
    await page.getByRole('button', { name: '放大查看 运营概览' }).click();
    const dialog = page.getByRole('dialog');
    checkDonut(dialog.getByRole('img', { name: /^存储构成:/ }).element());
    expect(dialog.element().scrollWidth).toBeLessThanOrEqual(width);
    if (value === 123456.78) await page.screenshot({ path: `__screenshots__/distribution-usd-dialog-${width}.png` });
    await userEvent.keyboard('{Escape}');
  }
});

it.each([1440, 736, 390, 320])('native composition renders without frames or overflow at %i', async width => {
  await page.viewport(width, 1000);
  const { container } = render(<main style={{ maxInlineSize: 1000, padding: 12 }}><NativeReportView payload={payload} /></main>);
  expect(container.querySelector('iframe')).toBeNull();
  expect(container.querySelectorAll('svg').length).toBeGreaterThanOrEqual(2);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.getByRole('button', { name: '合计', exact: true }).click();
  const paths = [...container.querySelectorAll('svg path')];
  expect(paths.some(path => path.getBoundingClientRect().width > 100)).toBe(true);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  expect(container.querySelector('script')).toBeNull();
  expect(container.textContent).toContain('<script>alert(1)</script>');
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
});

it('wide inspection is a native accessible dialog, not an embedded page', async () => {
  await page.viewport(1440, 1000);
  render(<NativeReportView payload={payload} />);
  await page.getByRole('button', { name: '放大查看 运营概览' }).click();
  await expect.element(page.getByRole('dialog', { name: '运营概览' })).toBeVisible();
  expect(document.querySelector('iframe')).toBeNull();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
});

it.each([1440, 390, 320])('keeps Close visible for an accepted unbroken title at %i', async width => {
  await page.viewport(width, 1000);
  render(<NativeReportView payload={{ ...payload, title: 'X'.repeat(200) }} />);
  await page.getByRole('button', { name: `放大查看 ${'X'.repeat(200)}` }).click();
  const close = document.querySelector<HTMLElement>('[role="dialog"] button[aria-label="Close"]')!;
  const box = close.getBoundingClientRect();
  expect(box.left).toBeGreaterThanOrEqual(0);
  expect(box.right).toBeLessThanOrEqual(width);
  expect(document.querySelector('[role="dialog"]')!.scrollWidth).toBeLessThanOrEqual(width);
});

it('keeps a view backlink beside its composition without consuming another row', async () => {
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

it.each([1440, 390, 320])('keeps long review disclosures intact without overflow at %i', async width => {
  await page.viewport(width, 1000);
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected records fixture');
  records.datasets[0].items[0].disclosures = [{ id: 'review', label: 'Full review', body: '字'.repeat(8000), note: 'Publisher note', tone: 'neutral' }];
  render(<main style={{ maxInlineSize: 1000, padding: 12 }}><NativeReportView payload={view} /></main>);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  await page.getByRole('button', { name: 'Full review', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Full review', exact: true }).element();
  expect(panel.querySelector('blockquote')?.textContent).toBe('字'.repeat(8000));
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.getByRole('button', { name: '放大查看 运营概览' }).click();
  const dialog = page.getByRole('dialog');
  await expect.element(dialog.getByRole('region', { name: 'Full review', exact: true })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('button', { name: '放大查看 运营概览' })).toHaveFocus();
});

it('returns focus to the selected record after closing its detail', async () => {
  await page.viewport(1440, 1000);
  render(<ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'inline', kind: 'view', payload }] }} empty={null} />);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  await page.getByRole('button', { name: '收起详情', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '查看详情', exact: true })).toHaveFocus();
});

it('uses the template\'s narrow-summary ratio for the App\'s performance units', async () => {
  await page.viewport(1440, 1000);
  const { container } = render(<main style={{ inlineSize: 1120, padding: 16 }}><NativeReportView payload={performance} resolveOverlay={resolveDemo} /></main>);
  const children = container.querySelector('section > div')!.children;
  expect(children[1].getBoundingClientRect().width / children[0].getBoundingClientRect().width).toBeGreaterThan(1.8);
  const slider = page.getByRole('slider', { name: '总资产变化 观察日期' }).element() as HTMLInputElement;
  expect(getComputedStyle(slider).opacity).toBe('0');
  slider.focus();
  await userEvent.keyboard('{Home}');
  expect(slider.value).toBe('0');
  expect(slider.getAttribute('aria-valuetext')).toContain('2026-07-01');
  expect(container.querySelector('iframe')).toBeNull();
});

it('stacks record details according to their own cell width, not the whole composition', async () => {
  await page.viewport(1440, 1000);
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  view.rows = [{ id: 'narrow', title: 'Narrow records', layout: 'three', cells: [records, view.rows[0].cells[0], view.rows[1].cells[0]] }];
  render(<main style={{ inlineSize: 1000 }}><NativeReportView payload={view} /></main>);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  const detail = page.getByRole('region', { name: '备份是否按时完成？ 详情' }).element();
  expect(detail.getBoundingClientRect().width).toBeGreaterThan(180);
  expect(detail.getBoundingClientRect().right).toBeLessThanOrEqual(detail.parentElement!.getBoundingClientRect().right);
});

it('contains long observation tooltips inside the plot without covering the legend', async () => {
  await page.viewport(1440, 1000);
  const { container } = render(<div style={{ inlineSize: 350 }}><TimeSeriesChart label="测量" emptyText="无数据"
    selection={{ datasetId: 'long', selected: null, sample: null, readoutOpen: false }} onSelection={() => {}}
    datasets={[{ id: 'long', label: '长说明', unit: 'GB', style: 'line',
      series: Array.from({ length: 6 }, (_, i) => ({ id: `s${i}`, label: `Series ${i} ${'long descriptive label '.repeat(4)}`, palette: i + 1 })),
      points: [{ date: '2026-09-23', values: [1, 2, 3, 4, 5, 6] }] }]} /></div>);
  const cursor = page.getByRole('slider').element();
  await page.getByRole('slider').hover();
  const tooltip = container.querySelector('[aria-hidden="true"][style]')!;
  expect(tooltip.getBoundingClientRect().bottom).toBeLessThanOrEqual(cursor.getBoundingClientRect().bottom);
});

it('keeps summary and curve beside each other in an ordinary report slot', async () => {
  await page.viewport(1440, 1000);
  const { container } = render(<main style={{ inlineSize: 700 }}><NativeReportView payload={performance} resolveOverlay={resolveDemo} /></main>);
  const cells = container.querySelector('section > div')!.children;
  expect(Math.abs(cells[0].getBoundingClientRect().top - cells[1].getBoundingClientRect().top)).toBeLessThan(2);
  expect(cells[1].getBoundingClientRect().width).toBeGreaterThan(cells[0].getBoundingClientRect().width);
});

it('reaches disclosures and snapshot controls using the unchanged shared dialog', async () => {
  await page.viewport(1440, 1000);
  render(<NativeReportView payload={payload} />);
  await page.getByRole('button', { name: '查看详情', exact: true }).click();
  await page.getByRole('button', { name: '放大查看 运营概览' }).click();
  const detailClose = page.getByRole('button', { name: '收起详情', exact: true }).element() as HTMLElement;
  detailClose.focus();
  await userEvent.keyboard('{Tab}');
  const records = payload.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected records fixture');
  expect(document.activeElement?.textContent).toContain(records.datasets[0].items[0].disclosures[0].label);
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('dialog').getByText('<script>alert(1)</script>', { exact: true })).toBeVisible();
  await userEvent.keyboard('{Tab}');
  expect(document.activeElement?.textContent).toContain('快照信息');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('dialog').getByText(/operations-r1/)).toBeVisible();
});

it('keeps the App fill timestamps readable in a local table scroller at 320px', async () => {
  await page.viewport(320, 1000);
  const decisions = nativeViewPayloadSchema.parse(demo.views[2]);
  const { container } = render(<main style={{ inlineSize: 288, margin: 16 }}><NativeReportView payload={decisions} resolveOverlay={resolveDemo} /></main>);
  const table = container.querySelector('table')!;
  const scroller = table.parentElement!;
  const timestamp = table.querySelector('tbody td:last-child')!;
  expect(timestamp.getBoundingClientRect().width).toBeGreaterThan(120);
  expect(scroller.scrollWidth).toBeGreaterThan(scroller.clientWidth);
  scroller.scrollLeft = scroller.scrollWidth;
  expect(timestamp.getBoundingClientRect().right).toBeLessThanOrEqual(scroller.getBoundingClientRect().right + 1);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(320);
});

it('gives the App holdings a full row below the charts in an intermediate slot', async () => {
  await page.viewport(768, 1000);
  const allocation = nativeViewPayloadSchema.parse(demo.views[1]);
  const { container } = render(<main style={{ inlineSize: 700 }}><NativeReportView payload={allocation} resolveOverlay={resolveDemo} /></main>);
  const cells = container.querySelector('section > div')!.children;
  const first = cells[0].getBoundingClientRect();
  const holdings = cells[2].getBoundingClientRect();
  expect(holdings.top).toBeGreaterThan(first.bottom);
  expect(holdings.width).toBeGreaterThan(first.width * 1.8);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(768);
});
