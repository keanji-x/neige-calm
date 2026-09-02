import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { NetworkPane } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('keeps one row shape and one trailing edge at phone width', async () => {
    await page.viewport(390, 844);
    const { container } = render(<NetworkPane
      settings={{}}
      loadError={null}
      saving={false}
      saveError={null}
      savedAt={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
    />);

    /*
     * `expect(locator).toBeTruthy()` — which these three assertions used to be
     * — cannot fail: a locator object is truthy whether or not it matches
     * anything. `expect.element` is the form that actually queries the page.
     */
    await expect.element(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeInTheDocument();
    await expect.element(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeInTheDocument();
    // No Save button: a proxy commits when its field is left (see `public.tsx`).
    expect(container.querySelectorAll('button')).toHaveLength(0);
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});

describe('Settings confirmation and example text', () => {
  /*
   * Two claims a rendering engine has to answer, both about *paint*:
   *
   *   1. the confirmation is a tick and the word `Saved.` is not on screen —
   *      jsdom reports every element as visible, so "hidden" is unfalsifiable
   *      there and the word could return without a test noticing;
   *   2. a placeholder is a lighter tone than a value the reader typed —
   *      jsdom parses `::placeholder` and then declines to compute it.
   */
  it('confirms with a tick whose word is only in the live region', async () => {
    await page.viewport(1180, 640);
    const view = render(<NetworkPane
      settings={{}} loadError={null} saving={false} saveError={null} savedAt={null}
      onSave={vi.fn()} onRetryLoad={vi.fn()}
    />);
    // Driven through real user events: the commit is a React `onBlur`, and a
    // raw `input.value = …` does not reach React's own value tracker.
    await page.getByRole('textbox', { name: 'HTTP proxy' }).fill('http://edge:3128');
    await page.getByRole('textbox', { name: 'HTTPS proxy' }).click();
    view.rerender(<NetworkPane
      settings={{}} loadError={null} saving={false} saveError={null} savedAt={1234}
      onSave={vi.fn()} onRetryLoad={vi.fn()}
    />);
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

    // Announced: exactly one live region, carrying the word.
    const announced = [...document.querySelectorAll('[role="status"]')];
    expect(announced.map((node) => node.textContent)).toEqual(['Saved.']);

    /*
     * Not drawn — and asserted over *every* node that says the word, not just
     * the first. Reading one node cannot hold this claim: putting the message
     * back on the field's own status renders a second `Saved.` beside the tick
     * while the hidden region still matches `querySelector`, so the earlier
     * single-node version of this test stayed green through exactly the
     * regression it exists to catch.
     */
    const painted = [...document.querySelectorAll('*')].filter((node) =>
      node.textContent === 'Saved.' && node.children.length === 0);
    expect(painted.length).toBeGreaterThan(0);
    for (const node of painted) {
      const box = node.getBoundingClientRect();
      expect(Math.round(box.width)).toBeLessThanOrEqual(1);
      expect(Math.round(box.height)).toBeLessThanOrEqual(1);
    }
    // The tick itself is what the reader sees, so it must actually be painted.
    const tick = document.querySelector('input')?.closest('div')?.querySelector('svg');
    expect(tick === null || tick === undefined ? 0 : tick.getBoundingClientRect().width)
      .toBeGreaterThan(0);
  });

  it('paints an example lighter than a value', async () => {
    await page.viewport(1180, 640);
    render(<NetworkPane
      settings={{ http_proxy: 'http://typed:3128' }} loadError={null} saving={false}
      saveError={null} savedAt={null} onSave={vi.fn()} onRetryLoad={vi.fn()}
    />);
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

    const [filled, empty] = [...document.querySelectorAll<HTMLInputElement>('input')];
    if (filled === undefined || empty === undefined) throw new Error('expected two proxy fields');
    const lightness = (color: string) => {
      const match = /oklch\(([\d.]+)/.exec(color);
      if (match?.[1] === undefined) throw new Error(`not an oklch colour: ${color}`);
      return Number(match[1]);
    };
    const value = lightness(getComputedStyle(filled).color);
    const example = lightness(getComputedStyle(empty, '::placeholder').color);
    // Lighter on a light background means *closer to the paper*, and by a step
    // the eye reads as a different kind of text rather than as emphasis.
    expect(example).toBeGreaterThan(value + 0.2);
  });
});
