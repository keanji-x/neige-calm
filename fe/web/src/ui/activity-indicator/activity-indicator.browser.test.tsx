import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { ActivityIndicator } from './public.tsx';

afterEach(() => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

type Rgb = readonly [number, number, number];

/*
 * Chromium serialises a computed colour in the space it was authored in, so
 * the string tells us nothing about the pixel. Painting it does: canvas runs
 * the same parse and gamut mapping as the compositor.
 */
function paintedRgb(cssColor: string): Rgb {
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (context === null) throw new Error('no 2d canvas context');
  context.fillStyle = cssColor;
  context.fillRect(0, 0, 1, 1);
  const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
  return [r, g, b];
}

/** sRGB bytes → OKLCH hue in degrees, the axis the palette is specified on. */
function oklchHue([R, G, B]: Rgb): number {
  const channel = (v: number) => {
    const c = v / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  const [r, g, b] = [R, G, B].map(channel);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return ((Math.atan2(bb, a) * 180) / Math.PI + 360) % 360;
}

function hueDistance(first: number, second: number): number {
  const raw = Math.abs(first - second) % 360;
  return Math.min(raw, 360 - raw);
}

function backgroundOf(state: 'attention' | 'failed'): Rgb {
  const { container } = render(<ActivityIndicator state={state} />);
  const dot = container.querySelector<HTMLElement>('[data-nc-activity]');
  if (dot === null || dot.getAttribute('data-nc-activity') !== state) throw new Error(`no ${state} dot`);
  return paintedRgb(getComputedStyle(dot).backgroundColor);
}

describe('activity indicator colours', () => {
  /*
   * #1722 decision (b): amber for "waiting on you", red for "broken". Before
   * the re-hue the two families sat 5° apart on the OKLCH wheel and the two
   * dots were indistinguishable; this pins the distance as the engine paints
   * it, in both themes, so `.failed` cannot quietly fall back to `--warn`
   * (or the palette drift back together) without this reddening.
   */
  it.each(['light', 'dark'] as const)('%s: attention and failed sit more than 30° apart in OKLCH hue', (theme) => {
    if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
    const attention = backgroundOf('attention');
    document.body.replaceChildren();
    const failed = backgroundOf('failed');
    expect(attention).not.toEqual(failed);
    expect(hueDistance(oklchHue(attention), oklchHue(failed))).toBeGreaterThan(30);
  });

  it('renders nothing for quiet and a distinct marker for every other state', () => {
    const { container } = render(<>
      <ActivityIndicator state="quiet" />
      <ActivityIndicator state="unread" />
      <ActivityIndicator state="working" />
      <ActivityIndicator state="attention" />
      <ActivityIndicator state="failed" />
    </>);
    expect([...container.querySelectorAll('[data-nc-activity]')].map((node) => node.getAttribute('data-nc-activity')))
      .toEqual(['unread', 'working', 'attention', 'failed']);
  });
});
