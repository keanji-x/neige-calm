import '../../styles/entry.css';
import { setup } from './mobile-browser-fixture.tsx';
import { cleanup } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

declare module 'vitest/browser' {
  interface BrowserCommands {
    nativeEdgeTouch(type: 'touchStart' | 'touchMove' | 'touchEnd' | 'touchCancel', x: number): Promise<void>;
  }
}
afterEach(cleanup);
function edgeNode() {
  const edge = [...document.querySelectorAll('div')].find(node => getComputedStyle(node).touchAction === 'pan-y' && getComputedStyle(node).position === 'fixed');
  if (edge === undefined) throw new Error('Production EdgeSwipe missing');
  edge.setAttribute('data-test-native-edge', '');
  return edge;
}
const historyOpen = () => document.querySelector<HTMLDialogElement>('dialog#mobile-conversation-history')?.open ?? false;

it('invalidates native touch origin across Settings disabled and enabled commits', async () => {
  await page.viewport(390, 844);
  const router = setup('/track/w1');
  await page.getByRole('button', { name: 'Open conversation history' }).findElement();
  const edge = edgeNode();
  const events: string[] = [];
  for (const type of ['pointerdown', 'pointerup', 'pointercancel']) edge.addEventListener(type, event => events.push(event.type));
  await commands.nativeEdgeTouch('touchStart', 4);
  expect(events).toEqual(['pointerdown']);
  await router.navigate({ to: '/settings' });
  await expect.poll(() => edge.hidden).toBe(true);
  await router.navigate({ to: '/track/w1' });
  await expect.poll(() => edge.hidden).toBe(false);
  expect(edgeNode()).toBe(edge);
  await commands.nativeEdgeTouch('touchMove', 100);
  await commands.nativeEdgeTouch('touchEnd', 100);
  expect(events).toEqual(['pointerdown', 'pointerup']);
  await new Promise(requestAnimationFrame);
  expect(historyOpen()).toBe(false);
  await commands.nativeEdgeTouch('touchStart', 4);
  await commands.nativeEdgeTouch('touchMove', 100);
  await commands.nativeEdgeTouch('touchEnd', 100);
  await expect.poll(historyOpen).toBe(true);
});

it('native cancel consumes origin and a new native down remains admitted', async () => {
  await page.viewport(390, 844);
  setup('/track/w1');
  await page.getByRole('button', { name: 'Open conversation history' }).findElement();
  edgeNode();
  await commands.nativeEdgeTouch('touchStart', 4);
  await commands.nativeEdgeTouch('touchCancel', 4);
  expect(historyOpen()).toBe(false);
  await commands.nativeEdgeTouch('touchStart', 4);
  await commands.nativeEdgeTouch('touchMove', 100);
  await commands.nativeEdgeTouch('touchEnd', 100);
  await expect.poll(historyOpen).toBe(true);
});
