import { useState } from '../state/public.ts';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { DistributionChart, TimeSeriesChart, type PlotDataset, type PlotSelection } from './public.tsx';

afterEach(cleanup);

function Chart() {
  const [selection, onSelection] = useState<PlotSelection>({ datasetId: 'precise', selected: null, sample: null, readoutOpen: false });
  const dataset: PlotDataset = { id: 'precise', label: 'Precise', unit: 'GB', style: 'line',
    series: Array.from({ length: 6 }, (_, i) => ({ id: `s${i}`, label: `Series ${i} ${'longlabel'.repeat(8)}`, palette: i + 1 })),
    points: [{ date: '2026-09-22', values: [0, null, 1, 2, 3, 4] },
      { date: '2026-09-23', values: Array.from({ length: 6 }, () => 123456789.12345678) }] };
  return <TimeSeriesChart label="Measurement" emptyText="Empty" selection={selection} onSelection={onSelection}
    datasets={[dataset, { ...dataset, id: 'other', label: 'Other', series: dataset.series.slice(0, 1),
      points: dataset.points.map(point => ({ ...point, values: point.values.slice(0, 1) })) }]} />;
}

function expectTextInside(element: Element, box: DOMRect) {
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    const range = document.createRange();
    range.selectNodeContents(walker.currentNode);
    for (const rect of range.getClientRects()) {
      expect(rect.left).toBeGreaterThanOrEqual(box.left - 1);
      expect(rect.right).toBeLessThanOrEqual(box.right + 1);
      expect(rect.top).toBeGreaterThanOrEqual(box.top - 1);
      expect(rect.bottom).toBeLessThanOrEqual(box.bottom + 1);
    }
  }
}

it.each([
  { width: 180, columns: 1 }, { width: 220, columns: 1 }, { width: 320, columns: 1 },
  { width: 660, columns: 1 }, { width: 660, columns: 3 },
])('reads every exact observation with pointer and keyboard at $width / $columns columns', async ({ width, columns }) => {
  await page.viewport(1000, 1000);
  const { container } = render(<div style={{ inlineSize: width, display: 'grid', gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` }}><Chart /></div>);
  const slider = page.getByRole('slider');
  const closedHeight = container.getBoundingClientRect().height;
  await slider.hover();
  const disclosure = page.getByRole('button', { name: 'Measurement 观察值' });
  (disclosure.element() as HTMLButtonElement).focus();
  await userEvent.keyboard('{Enter}');
  const initialHeight = container.getBoundingClientRect().height;
  const readout = page.getByRole('region', { name: 'Measurement 观察值' }).element() as HTMLElement;
  expect(readout.getBoundingClientRect().height).toBe(144);
  expect(disclosure.element().getAttribute('aria-expanded')).toBe('true');
  const values = readout.querySelectorAll('dd');
  expect(values).toHaveLength(6);
  const input = slider.element() as HTMLInputElement;
  input.focus();
  await userEvent.keyboard('{End}');
  for (const value of values) {
    expect(value.textContent).toBe('123,456,789.12345678 GB');
    value.scrollIntoView({ block: 'nearest' });
    expectTextInside(value, readout.getBoundingClientRect());
  }
  for (const button of readout.querySelectorAll('button')) {
    button.focus();
    expectTextInside(button, readout.getBoundingClientRect());
    expect(document.getElementById(button.getAttribute('aria-describedby')!)?.textContent).toBe('123,456,789.12345678 GB');
  }
  await userEvent.keyboard('{Enter}');
  expect(readout.querySelectorAll('button')[5].getAttribute('aria-pressed')).toBe('true');
  expect(readout.querySelectorAll('dd')).toHaveLength(6);
  input.focus();
  await userEvent.keyboard('{Home}');
  expect(values[0].textContent).toBe('0 GB');
  expect(values[1].textContent).toBe('未知 GB');
  await userEvent.keyboard('{ArrowRight}');
  expect(values[0].textContent).toBe('123,456,789.12345678 GB');
  const bounds = input.getBoundingClientRect();
  await slider.hover({ position: { x: 2, y: 20 } });
  await expect.poll(() => input.value).toBe('0');
  const tooltip = container.querySelector<HTMLElement>('[aria-hidden="true"][style]')!;
  expect(getComputedStyle(tooltip).pointerEvents).toBe('none');
  expect(document.elementFromPoint(bounds.left + 10, bounds.top + 20)).toBe(input);
  await page.getByRole('button', { name: 'Other', exact: true }).click();
  expect(container.getBoundingClientRect().height).toBe(initialHeight);
  expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(width);
  readout.scrollTop = 0;
  await page.screenshot({ path: `__screenshots__/readout-${width}-${columns}.png` });
  await disclosure.click();
  await expect.element(page.getByRole('region', { name: 'Measurement 观察值' })).not.toBeInTheDocument();
  expect(container.getBoundingClientRect().height).toBe(closedHeight);
});

it('wraps distribution legend labels and exact values at 180px', () => {
  const { container } = render(<div style={{ inlineSize: 180 }}><DistributionChart label="Distribution" unit="GB"
    emptyText="Empty" selected={null} onSelect={() => {}} slices={Array.from({ length: 6 }, (_, i) => ({
      id: `${i}`, label: 'longlabel'.repeat(8), value: 123456789.12345678, palette: i + 1,
    }))} /></div>);
  for (const button of container.querySelectorAll('button')) expectTextInside(button, button.getBoundingClientRect());
  expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(180);
});
