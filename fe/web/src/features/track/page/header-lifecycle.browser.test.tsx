import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { renderPage, track } from './test-fixtures.tsx';

afterEach(() => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

type Rgb = readonly [number, number, number];

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

function contrast(first: Rgb, second: Rgb): number {
  const channel = (byte: number) => {
    const value = byte / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const luminance = ([r, g, b]: Rgb) =>
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
  const [high, low] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
}

describe('the track lifecycle label in the page header', () => {
  it('sits directly beside the title as quiet label-sized text', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ title: 'Status preview', lifecycle: 'working' }) });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;
    const gap = status.getBoundingClientRect().left - title.getBoundingClientRect().right;

    expect(gap).toBeGreaterThanOrEqual(4);
    expect(gap).toBeLessThanOrEqual(12);
    expect(getComputedStyle(status).fontSize).toBe('11px');
  });

  it('truncates a long title before the label can overlap trailing actions', async () => {
    /* Stay just above the 60rem mobile navigation cutover: below it the
       desktop PageHeader is intentionally replaced by MobileHeader. */
    await browserPage.viewport(1000, 600);
    renderPage({
      track: track({ title: 'A deliberately long track title '.repeat(12), lifecycle: 'working' }),
    });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;
    const remove = document.querySelector<HTMLElement>('[aria-label^="Delete track "]')!;
    const titleBox = title.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();

    expect(title.scrollWidth).toBeGreaterThan(title.clientWidth);
    expect(statusBox.left - titleBox.right).toBeGreaterThanOrEqual(4);
    expect(statusBox.left - titleBox.right).toBeLessThanOrEqual(12);
    expect(statusBox.right).toBeLessThan(remove.getBoundingClientRect().left);
  });

  it('keeps the subdued running colour readable in both themes', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ lifecycle: 'working' }) });
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;

    for (const theme of ['light', 'dark'] as const) {
      document.documentElement.dataset.theme = theme;
      const foreground = paintedRgb(getComputedStyle(status).color);
      const background = paintedRgb(getComputedStyle(document.body).backgroundColor);
      expect(contrast(foreground, background), theme).toBeGreaterThanOrEqual(4.5);
    }
  });
});
