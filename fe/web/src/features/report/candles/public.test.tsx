// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ChartCandlesPayload } from '../../../../../core/domain/report.ts';
import { ReportCandlesBlock } from './public.tsx';

afterEach(cleanup);

const DAY = 86_400_000;

function payload(overrides: Partial<ChartCandlesPayload> = {}): ChartCandlesPayload {
  return {
    symbol: '600519.SH',
    candles: [
      // rising, then falling
      [0, 100, 106, 99, 105, 1000],
      [DAY, 105, 107, 100, 101, 1200],
    ],
    ...overrides,
  };
}

describe('ReportCandlesBlock', () => {
  // The chart is drawn in SVG rather than on a canvas so that the palette is
  // the app's tokens. A hex colour here would be the start of the second
  // palette the legacy chart has to keep in sync by hand.
  it('draws in SVG and paints from tokens, never from literal colours', () => {
    const { container } = render(<ReportCandlesBlock payload={payload()} />);
    expect(container.querySelector('svg')).toBeTruthy();
    expect(container.innerHTML).not.toMatch(/#[0-9a-f]{6}/i);
  });

  // Fill is a second encoding channel on top of hue: red/green alone is not
  // readable under the most common colour-vision deficiency.
  it('marks up and down candles by class, so hollow/solid can be a rule and not a colour', () => {
    const { container } = render(<ReportCandlesBlock payload={payload()} />);
    const groups = [...container.querySelectorAll('svg > g > g')];
    expect(groups.length).toBe(2);
    expect(groups[0]?.getAttribute('class')).not.toBe(groups[1]?.getAttribute('class'));
  });

  it('describes the whole figure once instead of announcing every candle', () => {
    const { container } = render(<ReportCandlesBlock payload={payload()} />);
    expect(screen.getByRole('img').getAttribute('aria-label')).toContain('600519.SH');
    expect(container.querySelector('svg > g')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('filters the range client-side, since the data is already inlined', async () => {
    const long = Array.from({ length: 400 }, (_, index): ChartCandlesPayload['candles'][number] =>
      [index * DAY, 100, 101, 99, 100, 10]);
    const { container } = render(<ReportCandlesBlock payload={payload({ candles: long })} />);
    expect(container.querySelectorAll('svg > g > g').length).toBe(400);
    await userEvent.click(screen.getByRole('button', { name: '1M' }));
    expect(container.querySelectorAll('svg > g > g').length).toBe(31);
  });

  // Two candles is the payload's own floor. A range that filters below it would
  // draw an empty box, which says "no data" about a series that has plenty.
  it('keeps the full series when a range would leave fewer than two candles', async () => {
    const sparse: ChartCandlesPayload['candles'] = [
      [0, 100, 101, 99, 100, 10],
      [400 * DAY, 100, 101, 99, 100, 10],
    ];
    const { container } = render(<ReportCandlesBlock payload={payload({ candles: sparse })} />);
    await userEvent.click(screen.getByRole('button', { name: '1M' }));
    expect(container.querySelectorAll('svg > g > g').length).toBe(2);
  });
});
