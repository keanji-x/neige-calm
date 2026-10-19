// Browser readiness and screenshot export only; schema assertions stay separate.
import { expect } from 'vitest';
import { page } from 'vitest/browser';

export async function waitForEmptySurfaceRendering() {
  await expect.poll(() => document.getAnimations().filter(a => a.effect?.getTiming().iterations !== Infinity).every(a => a.playState !== 'running')).toBe(true);
  await document.fonts.ready;
}

export async function captureEmptySurface(name: string) {
  await page.screenshot({ path: './__screenshots__/empty-catalog/' + name + '.png' });
}
