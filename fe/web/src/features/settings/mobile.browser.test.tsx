import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { NetworkPane } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('keeps one row shape and one trailing edge at phone width', async () => {
    await page.viewport(390, 844);
    render(<NetworkPane
      onOpenMobile={vi.fn()}
      settings={{}}
      loadError={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
    />);

    /* `expect.element` actually queries the page; `expect(locator).toBeTruthy()` cannot fail. */
    await expect.element(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeInTheDocument();
    await expect.element(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeInTheDocument();
    await expect.element(page.getByRole('button', { name: 'Save', exact: true })).not.toBeInTheDocument();
    await expect.element(page.getByRole('button', { name: /Mobile connection/ })).toBeInTheDocument();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});

describe('Settings confirmation and example text', () => {
  it('confirms with a tick whose word is only in the live region', async () => {
    await page.viewport(1180, 640);
    render(<NetworkPane
      onOpenMobile={vi.fn()}
      settings={{}} loadError={null} onSave={() => Promise.resolve()} onRetryLoad={vi.fn()}
    />);
    // Driven through real user events: the commit is a React `onBlur`, and a
    // raw `input.value = …` does not reach React's own value tracker.
    await page.getByRole('textbox', { name: 'HTTP proxy' }).fill('http://edge:3128');
    await page.getByRole('textbox', { name: 'HTTPS proxy' }).click();
    await new Promise<void>((resolve) => setTimeout(resolve, 50));
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

    const announced = [...document.querySelectorAll('[role="status"]')].map((n) => n.textContent);
    expect(announced).toEqual(['Saved.', '']);

    const painted = [...document.querySelectorAll('*')].filter((node) =>
      node.textContent === 'Saved.' && node.children.length === 0);
    expect(painted.length).toBeGreaterThan(0);
    for (const node of painted) {
      const box = node.getBoundingClientRect();
      expect(Math.round(box.width)).toBeLessThanOrEqual(1);
      expect(Math.round(box.height)).toBeLessThanOrEqual(1);
    }
    const tick = document.querySelector('input')?.closest('div')?.querySelector('svg');
    expect(tick === null || tick === undefined ? 0 : tick.getBoundingClientRect().width)
      .toBeGreaterThan(0);
  });

  it('paints an example lighter than a value', async () => {
    await page.viewport(1180, 640);
    render(<NetworkPane
      onOpenMobile={vi.fn()}
      settings={{ http_proxy: 'http://typed:3128' }} loadError={null}
      onSave={vi.fn()} onRetryLoad={vi.fn()}
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
    expect(example).toBeGreaterThan(value + 0.2);
  });
});
