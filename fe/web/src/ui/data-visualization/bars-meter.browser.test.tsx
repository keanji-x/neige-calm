import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { BarChart, MeterChart, MetricGroup } from './public.tsx';

afterEach(cleanup);
it.each([1440, 390, 320])('contains publisher text, signed bars and meters at %i', async width => {
  await page.viewport(width, 1000);
  const label = 'Long publisher label '.repeat(10);
  const { container } = render(<main style={{ maxInlineSize: 600, padding: 12 }}>
    <MetricGroup items={[{ id: 'text', label, value: { state: 'text', text: 'Awaiting collection '.repeat(15) }, detail: label, tone: 'neutral', emphasis: 'primary' }]} />
    <BarChart label="Observed changes" unit="GB" emptyText="No observations" points={[
      { label, value: 50, tone: 'negative' }, { label: 'Saving', value: -20, tone: 'positive' },
    ]} />
    <MeterChart label="Storage" unit="GB" used={150} limit={100} usedLabel={label} limitLabel="Capacity"
      detail="Publisher interpretation" emptyText="No observations" tone="warning" />
  </main>);
  await expect.element(page.getByRole('img', { name: /Observed changes/ })).toBeVisible();
  await expect.element(page.getByRole('meter', { name: 'Storage' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  expect(container.querySelector('iframe')).toBeNull();
});
