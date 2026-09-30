// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { BarChart, MeterChart, MetricGroup, signedBarLayout } from './public.tsx';

afterEach(cleanup);
it('draws signed geometry independently of publisher tones', () => {
  expect(signedBarLayout([0])).toEqual([{ start: 0, width: 0, zero: 0 }]);
  expect(signedBarLayout([50])).toEqual([{ start: 0, width: 100, zero: 0 }]);
  expect(signedBarLayout([-100, 50])).toEqual([{ start: 0, width: 50, zero: 50 }, { start: 50, width: 25, zero: 50 }]);
  render(<BarChart label="Cost change" unit="USD" emptyText="No observations" points={[
    { label: 'Increase', value: 50, tone: 'negative' }, { label: 'Saving', value: -20, tone: 'positive' },
  ]} />);
  expect(screen.getByText('+50').className).toContain('negative');
  expect(screen.getByText('-20').className).toContain('positive');
  expect(screen.getByRole('img').getAttribute('aria-label')).toContain('-20');
});

it('preserves actual over-limit observations while capping only meter geometry', () => {
  render(<MeterChart label="Storage" unit="GB" used={150} limit={100} usedLabel="Observed" limitLabel="Capacity"
    detail="Publisher interpretation" emptyText="No observations" tone="warning" />);
  const meter = screen.getByRole('meter', { name: 'Storage' });
  expect(meter.getAttribute('value')).toBe('100');
  expect(meter.getAttribute('aria-valuetext')).toBe('150 / 100 GB');
  expect(screen.getByText('Observed 150')).toBeTruthy();
  expect(screen.getByText('Publisher interpretation')).toBeTruthy();
});

it.each([{ used: null, limit: 100 }, { used: 0, limit: null }, { used: null, limit: null }])('does not replace unknown meter observations with zero', values => {
  render(<MeterChart label="Storage" unit="GB" {...values} usedLabel="Observed" limitLabel="Capacity"
    detail="" emptyText="No observations" tone="neutral" />);
  expect(screen.getByText('No observations')).toBeTruthy();
  expect(screen.queryByRole('meter')).toBeNull();
});

it('retains a real zero meter value and precise small bar observations', () => {
  render(<><MeterChart label="Storage" unit="GB" used={0} limit={100} usedLabel="Observed" limitLabel="Capacity"
    detail="" emptyText="No observations" tone="neutral" />
    <BarChart label="Changes" unit="GB" emptyText="No observations" points={[{ label: 'Measured', value: 0.0001, tone: 'neutral' }]} /></>);
  expect(screen.getByRole('meter').getAttribute('value')).toBe('0');
  expect(screen.getByRole('img').getAttribute('aria-label')).toContain('0.0001');
});

it('renders publisher text metrics literally, without executable markup', () => {
  render(<MetricGroup items={[{ id: 'state', label: 'Collection', value: { state: 'text', text: '<script>ready</script>' },
    detail: 'Publisher wording', tone: 'neutral', emphasis: 'normal' }]} />);
  expect(screen.getByText('<script>ready</script>')).toBeTruthy();
  expect(document.querySelector('script')).toBeNull();
});
