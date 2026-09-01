import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { Drawer } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

/*
 * The drawer is measured **outside `.shell`** on purpose (#1191 §3.4).
 *
 * `--mobile-dock-h` is declared by `.shell` and by nothing else, so every other
 * mounting context — this test, a story, a route rendered bare — leaves it
 * undefined, and `calc(var(--mobile-dock-h) + …)` is then invalid at computed
 * value time: the whole declaration is dropped and the element falls back to
 * `inset: auto` / `block-size: auto`. The fix is the `, 0px` fallback, and this
 * is the only shape that can see it.
 *
 * The previous version of this file wrapped the drawer in a `<main>` with an
 * inline `blockSize: 100dvh` and asserted nothing about geometry. On compact the
 * drawer is `position: fixed`, whose containing block is the viewport — the
 * parent's height cannot reach it — so that wrapper proved nothing either way.
 */
describe('Drawer mobile Header', () => {
  it('opens a new Chat as Untitled with the shared Back-first Header', async () => {
    await page.viewport(390, 844);
    const onClose = vi.fn();
    render(
      <Drawer
        open
        title="Untitled"
        mobileBackLabel="Report"
        onClose={onClose}
        footer={<form aria-label="Chat composer"><textarea aria-label="Message" /></form>}
      >
        <p>Start a new conversation about this Report.</p>
      </Drawer>,
    );

    expect(page.getByRole('heading', { name: 'Untitled' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Back to Report' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-header]')).not.toBeNull();
    expect(document.querySelector('button[aria-label="Close conversation"]')).toBeNull();

    await settlePaint();
    // The slide-in animates `translate` only, but let it finish so the box is
    // read at rest.
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    await Promise.all(drawer.getAnimations().map((animation) => animation.finished));

    /*
     * With no `.shell` above it there is no dock to subtract, so the fallback
     * resolves to zero and the drawer is exactly the viewport. Drop the `, 0px`
     * and both declarations become invalid: `block-size` collapses to the
     * content's height and the fixed box no longer reaches the bottom edge.
     */
    const box = drawer.getBoundingClientRect();
    expect(getComputedStyle(drawer).blockSize).toBe(`${window.innerHeight}px`);
    expect(box.height).toBe(window.innerHeight);
    expect(box.top).toBe(0);
    expect(box.bottom).toBe(window.innerHeight);

    await page.screenshot({ path: '../../../../test-results/mobile-chat.png' });
  });
});
