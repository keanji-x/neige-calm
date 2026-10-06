import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { TrackClosedBadge } from './public.tsx';

afterEach(() => {
  cleanup(); document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

type Rgb = readonly [number, number, number];

/** The pixel the engine paints for a colour string, whatever space it was authored in. */
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

function tokenRgb(token: string): Rgb {
  const probe = document.createElement('span');
  probe.style.color = `var(${token})`;
  document.body.append(probe);
  const rgb = paintedRgb(getComputedStyle(probe).color);
  probe.remove();
  return rgb;
}

function closedBadgeRgb(): Rgb {
  const { container } = render(<TrackClosedBadge closedAt={1} />);
  const badge = container.querySelector<HTMLElement>('[data-testid="track-closed"]');
  if (badge === null) throw new Error('no badge');
  return paintedRgb(getComputedStyle(badge).color);
}

// Measured on the painted pixel in both themes, so a rule that quietly fell back to another family reddens here.
describe.each(['light', 'dark'] as const)('%s: closed badge tone', (theme) => {
  it('paints the closed badge neutral, apart from the warn and error families', () => {
    if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
    expect(closedBadgeRgb()).toEqual(tokenRgb('--text-3'));
    expect(closedBadgeRgb()).not.toEqual(tokenRgb('--warn-text'));
    expect(closedBadgeRgb()).not.toEqual(tokenRgb('--error-text'));
  });
});
