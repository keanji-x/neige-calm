import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { TrackLifecycleBadge } from './public.tsx';

afterEach(() => {
  document.body.replaceChildren();
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

function badgeRgb(lifecycle: 'failed' | 'blocked' | 'working'): Rgb {
  const { container } = render(<TrackLifecycleBadge lifecycle={lifecycle} />);
  const badge = container.querySelector<HTMLElement>('[data-testid="track-lifecycle"]');
  if (badge === null) throw new Error('no badge');
  return paintedRgb(getComputedStyle(badge).color);
}

/*
 * #1722 §5.3 — three tones, painted with the same two semantic families the
 * activity indicator uses: `failed` is `--error-text`, `blocked` / `reviewing`
 * are `--warn-text`, and a running phase is plain `--text-3`. Measured on the
 * painted pixel in both themes, so a `.failed` rule that quietly fell back to
 * the warn family reddens here.
 */
describe.each(['light', 'dark'] as const)('%s: lifecycle badge tones', (theme) => {
  it('paints failed with the error family, attention with the warn family, and a running phase neutral', () => {
    if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
    expect(badgeRgb('failed')).toEqual(tokenRgb('--error-text'));
    document.body.replaceChildren();
    expect(badgeRgb('blocked')).toEqual(tokenRgb('--warn-text'));
    document.body.replaceChildren();
    expect(badgeRgb('working')).toEqual(tokenRgb('--text-3'));
    expect(tokenRgb('--error-text')).not.toEqual(tokenRgb('--warn-text'));
  });
});
