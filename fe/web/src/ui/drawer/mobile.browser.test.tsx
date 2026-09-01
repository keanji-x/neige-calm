import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { Drawer, drawerSeamAround } from './public.tsx';

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

  /*
   * ── The seam is gone on a phone, and that is a product decision (#1191) ────
   *
   * A desktop drawer is a card with a strip of page beside it, and that strip —
   * the seam — is where `features/chat` portals the exchange rail. A compact
   * page has no such strip: the drawer is the whole viewport, so
   * `@media (width < 60rem)` sets `.seam { display: none }` and conversation
   * history is read from the Report's Conversations list instead.
   *
   * Until this case that removal was load-bearing and untested. It was also
   * *invisible*: nothing in `ui/drawer` renders anything into the seam, so the
   * rule could be deleted and every drawer test stayed green — while the one
   * suite that would have noticed, `features/chat/thread/thread.coarse.browser.
   * test.tsx`, was itself running at a phone viewport and reading the rail's
   * geometry as zeroes. Two features each right, colliding in a place neither
   * could see. The collision is now split in two and both halves are pinned:
   * that file measures the rail on a coarse **tablet**, where a finger and a
   * seam coexist, and this case owns the phone.
   *
   * Read as a box and not as a declaration. `display: none` is asserted on the
   * seam's *computed* style, and then a sized child is put inside it and
   * measured at zero — because "the rail is not painted" is a claim about what
   * a reader can see, and a child of a `display: none` parent computes its own
   * `display` perfectly normally (the trap `app/shell/drawer-seam.browser.
   * test.tsx` documents at length).
   *
   * The desktop half is asserted in the same case rather than trusted, because
   * a seam that never paints anywhere would satisfy the phone half on its own
   * and take the whole rail down with it silently.
   */
  it('paints no seam for the exchange rail on a phone, and does on a wide page', async () => {
    render(
      <Drawer open title="Chat" mobileBackLabel="Report" onClose={() => {}}>
        <p data-testid="transcript">the transcript</p>
      </Drawer>,
    );

    /* Found the way `features/chat` finds it — through the drawer's own
       cross-module accessor — so a seam that stopped being reachable at all
       fails here rather than being silently skipped. */
    const seam = drawerSeamAround(document.querySelector('[data-testid="transcript"]'));
    expect(seam).not.toBeNull();
    /* A stand-in for the rail: something with a real box, so a zero reading
       below is the seam's doing and not the probe's. */
    const rail = seam!.appendChild(document.createElement('div'));
    rail.style.inlineSize = '24px';
    rail.style.blockSize = '320px';

    await page.viewport(390, 844);
    await settlePaint();
    expect(getComputedStyle(seam!).display).toBe('none');
    expect(seam!.getBoundingClientRect().height).toBe(0);
    expect(seam!.getBoundingClientRect().width).toBe(0);
    /* The rail itself, which is the thing the reader would or would not see. */
    expect(rail.getBoundingClientRect().height).toBe(0);
    expect(rail.getBoundingClientRect().width).toBe(0);

    /* And above the breakpoint the same seam is a real strip of page, so the
       assertions above are about the media rule and not about an element that
       never has a box. */
    await page.viewport(1400, 900);
    await settlePaint();
    expect(getComputedStyle(seam!).display).not.toBe('none');
    expect(rail.getBoundingClientRect().height).toBe(320);
  });
});
