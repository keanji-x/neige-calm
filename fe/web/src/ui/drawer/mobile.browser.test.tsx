import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { useState } from '../state/public.ts';
import { Drawer, drawerSeamAround } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

/* Measured outside `.shell` on purpose: only `.shell` declares `--mobile-dock-h`, so this is the one shape that can see the `, 0px` fallback. */
describe('Drawer mobile Header', () => {
  function Harness() {
    const [isOpen, setOpen] = useState(false);
    return <><button type="button" onClick={() => setOpen(true)}>Open details</button>
      <Drawer open={isOpen} title="Details" mobileBackLabel="Report" onClose={() => setOpen(false)}><p>Details body</p></Drawer></>;
  }

  it('opens compact details directly without a page animation', async () => {
    await page.viewport(390, 844);
    const view = render(<Harness />);
    try {
      await page.getByRole('button', { name: 'Open details' }).click();
      const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
      expect(drawer.getAnimations()).toHaveLength(0);
      expect(drawer.getBoundingClientRect().left).toBe(0);
    } finally { view.unmount(); await page.viewport(1280, 720); }
  });

  it('closes compact details immediately and restores their opener', async () => {
    await page.viewport(390, 844);
    const view = render(<Harness />);
    try {
      const opener = await page.getByRole('button', { name: 'Open details' }).findElement();
      await page.elementLocator(opener).click();
      const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
      await page.getByRole('button', { name: 'Back to Report' }).click();
      expect(drawer.isConnected).toBe(false);
      await expect.poll(() => document.activeElement).toBe(opener);
    } finally { view.unmount(); await page.viewport(1280, 720); }
  });

  it('finishes a desktop close immediately when resized to compact', async () => {
    await page.viewport(1400, 900);
    const view = render(<Harness />);
    try {
      const opener = await page.getByRole('button', { name: 'Open details' }).findElement();
      await page.elementLocator(opener).click();
      const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
      await Promise.all(drawer.getAnimations().map((animation) => animation.finished));
      await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
      expect(drawer.isConnected).toBe(true);
      const exit = drawer.getAnimations()[0];
      expect(exit).toBeDefined(); exit.pause();
      await page.viewport(390, 844);
      await settlePaint();
      expect(drawer.isConnected).toBe(false);
      await expect.poll(() => document.activeElement).toBe(opener);
    } finally { view.unmount(); await page.viewport(1280, 720); }
  });

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
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(drawer.getAnimations()).toHaveLength(0);

    /* Without the `, 0px` fallback both declarations are invalid at computed-value time and the fixed box no longer reaches the bottom edge. */
    const box = drawer.getBoundingClientRect();
    expect(getComputedStyle(drawer).blockSize).toBe(`${window.innerHeight}px`);
    expect(box.height).toBe(window.innerHeight);
    expect(box.top).toBe(0);
    expect(box.bottom).toBe(window.innerHeight);

    await page.screenshot({ path: '../../../../test-results/mobile-chat.png' });
  });

  /* Asserted as a box, not a declaration: a child of a `display: none` parent computes its own `display` normally. The desktop half guards against a seam that never paints anywhere. */
  it('paints no seam for the exchange rail on a phone, and does on a wide page', async () => {
    render(
      <Drawer open title="Chat" mobileBackLabel="Report" onClose={() => {}}>
        <p data-testid="transcript">the transcript</p>
      </Drawer>,
    );

    const seam = drawerSeamAround(document.querySelector('[data-testid="transcript"]'));
    expect(seam).not.toBeNull();
    /* A stand-in with a real box, so a zero reading is the seam's doing and not the probe's. */
    const rail = seam!.appendChild(document.createElement('div'));
    rail.style.inlineSize = '24px';
    rail.style.blockSize = '320px';

    await page.viewport(390, 844);
    await settlePaint();
    expect(getComputedStyle(seam!).display).toBe('none');
    expect(seam!.getBoundingClientRect().height).toBe(0);
    expect(seam!.getBoundingClientRect().width).toBe(0);
    expect(rail.getBoundingClientRect().height).toBe(0);
    expect(rail.getBoundingClientRect().width).toBe(0);

    await page.viewport(1400, 900);
    await settlePaint();
    expect(getComputedStyle(seam!).display).not.toBe('none');
    expect(rail.getBoundingClientRect().height).toBe(320);
  });
});
