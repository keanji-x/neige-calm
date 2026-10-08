import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { ChatLayout } from '@astryxdesign/core/Chat';
import { ConversationSurface } from './mobile-conversation-surface.tsx';
import { ChatThread } from '../../features/chat/thread/public.tsx';
import type { TranscriptEntry } from '../../../../core/domain/conversation.ts';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });
const history: readonly TranscriptEntry[] = Array.from({ length: 12 }).flatMap((_, i) => [
  { id: `you-${i}`, author: 'you' as const, text: `Question ${i}`, atMs: i * 2000 },
  { id: `agent-${i}`, author: 'agent' as const, text: '这是一段用于验证手机滚动的回复。'.repeat(80), atMs: i * 2000 + 1 },
]);
function surface() {
  const close = vi.fn();
  render(<ConversationSurface mobileSheet open title="Current conversation" contextTitle="Neige Calm" onClose={close}
    footer={<div style={{ height: 80, background: 'white' }}><input aria-label="Draft" /></div>}>
    <ChatThread canContinue={false} cards={{}} stalled={false}
      conversation={{ id: 'chat', trackId: 'track', title: 'Current conversation', kind: 'codex', state: 'idle', updatedAt: 0 }} turns={history} />
  </ConversationSurface>);
  return close;
}
const scroller = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;

it('uses a single compact heading and grows to full screen without losing the draft', async () => {
  await page.viewport(390, 844);
  surface();
  await expect.element(page.getByRole('dialog', { name: 'Current conversation' })).toBeVisible();
  expect(page.getByRole('heading', { name: '对话', exact: true }).query()).toBeNull();
  await expect.element(page.getByRole('heading', { name: 'Neige Calm', exact: true })).toBeVisible();
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getBoundingClientRect().height).toBeGreaterThan(844 * 0.75);
  await page.getByRole('textbox', { name: 'Draft' }).fill('保留草稿');
  await page.getByRole('button', { name: 'Expand conversation' }).click();
  await expect.poll(() => panel.getBoundingClientRect().height).toBeGreaterThan(844 * 0.95);
  const viewport = window.visualViewport!;
  const descriptors = ['height', 'offsetTop'].map(key => Object.getOwnPropertyDescriptor(viewport, key));
  try {
    Object.defineProperty(viewport, 'height', { configurable: true, value: 480 });
    Object.defineProperty(viewport, 'offsetTop', { configurable: true, value: 70 });
    viewport.dispatchEvent(new Event('resize'));
    // Astryx adds a hidden overscroll skirt below the viewport; measure the
    // visible content rather than that decorative panel extension.
    const content = panel.querySelector<HTMLElement>('[data-nc-drawer]')!;
    await expect.poll(() => content.getBoundingClientRect().bottom).toBeLessThanOrEqual(550);
    expect(panel.getBoundingClientRect().top).toBeGreaterThanOrEqual(70);
    expect(page.getByRole('textbox', { name: 'Draft' }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(550);
  } finally {
    ['height', 'offsetTop'].forEach((key, index) => {
      const descriptor = descriptors[index];
      if (descriptor === undefined) Reflect.deleteProperty(viewport, key);
      else Object.defineProperty(viewport, key, descriptor);
    });
    viewport.dispatchEvent(new Event('resize'));
  }
  await page.getByRole('button', { name: 'Collapse conversation' }).click();
  await expect.poll(() => panel.getBoundingClientRect().height).toBeLessThan(844 * 0.9);
  await expect.element(page.getByRole('textbox', { name: 'Draft' })).toHaveValue('保留草稿');
});

it('anchors the return-to-bottom control to the message viewport and joins the composer', async () => {
  await page.viewport(390, 844);
  surface();
  await expect.poll(() => scroller().scrollTop).toBeGreaterThan(1000);
  await commands.wheelScroll('[data-nc-drawer-scroll]', -350);
  const dock = await page.getByRole('button', { name: 'Scroll to bottom' }).findElement();
  const initial = dock.getBoundingClientRect().bottom;
  await commands.wheelScroll('[data-nc-drawer-scroll]', -350);
  await expect.poll(() => dock.getBoundingClientRect().bottom).toBeCloseTo(initial, 0);
  const blur = document.querySelector<HTMLElement>('[data-nc-native-chat-layout]')!.lastElementChild!.children[1];
  const footer = document.querySelector<HTMLElement>('[data-nc-chat-footer]')!;
  expect(blur.getBoundingClientRect().bottom).toBeCloseTo(footer.getBoundingClientRect().top, 0);
  await page.getByRole('button', { name: 'Scroll to bottom' }).click();
  await expect.poll(() => scroller().scrollHeight - scroller().scrollTop - scroller().clientHeight).toBeLessThanOrEqual(1);
});

function swipe(target: Element, startY: number, endY: number) {
  const start = new Touch({ identifier: 1, target, clientX: 195, clientY: startY });
  const end = new Touch({ identifier: 1, target, clientX: 195, clientY: endY });
  target.dispatchEvent(new TouchEvent('touchstart', { bubbles: true, touches: [start], changedTouches: [start] }));
  target.dispatchEvent(new TouchEvent('touchmove', { bubbles: true, cancelable: true, touches: [end], changedTouches: [end] }));
  target.dispatchEvent(new TouchEvent('touchend', { bubbles: true, touches: [], changedTouches: [end] }));
}

it('leaves downward message swipes to scrolling instead of closing the conversation', async () => {
  await page.viewport(390, 844);
  const close = surface();
  await expect.element(page.getByRole('dialog', { name: 'Current conversation' })).toBeVisible();
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  scroller().scrollTop = 0;
  swipe(scroller(), 400, 720);
  await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  expect(close).not.toHaveBeenCalled();
  expect(panel.style.transform).toBe('');
});

it('keeps the header fixed when scrolling over the header and messages', async () => {
  await page.viewport(390, 844);
  surface();
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  expect(getComputedStyle(panel.lastElementChild!).overflowY).toBe('hidden');
  const heading = page.getByRole('heading', { name: 'Neige Calm', exact: true }).element();
  await expect.poll(() => scroller().scrollTop).toBeGreaterThan(1000);
  const top = heading.getBoundingClientRect().top;
  const position = scroller().scrollTop;
  await commands.wheelScroll('[data-nc-mobile-chat-panel] header', 600);
  await expect.poll(() => heading.getBoundingClientRect().top).toBeCloseTo(top, 0);
  await commands.wheelScroll('[data-nc-drawer-scroll]', -700);
  await expect.poll(() => scroller().scrollTop).toBeLessThan(position);
  expect(heading.getBoundingClientRect().top).toBeCloseTo(top, 0);
  for (let ancestor = scroller().parentElement; ancestor !== null && ancestor !== panel; ancestor = ancestor.parentElement) {
    expect(ancestor.scrollTop).toBe(0);
  }
  await expect.poll(() => document.querySelector('[data-nc-chat-scroll-dock]')).not.toBeNull();
  const blur = document.querySelector<HTMLElement>('[data-nc-native-chat-layout]')!.lastElementChild!.children[1];
  expect(blur.getBoundingClientRect().height).toBeGreaterThan(0);
  expect(getComputedStyle(blur).maskImage).toContain('linear-gradient');
});

it('uses the built-in compact ChatLayout blur without repainting its material', async () => {
  await page.viewport(390, 844);
  surface();
  await expect.element(page.getByRole('dialog', { name: 'Current conversation' })).toBeVisible();
  render(<ChatLayout data-testid="native-material-reference" density="compact" composer={null} scrollButton={<div />}
    style={{ position: 'absolute', left: 1000, width: 390, height: 200 }}>{null}</ChatLayout>);
  const layout = document.querySelector<HTMLElement>('[data-nc-native-chat-layout]')!;
  const reference = page.getByTestId('native-material-reference').element();
  const actual = getComputedStyle(layout.lastElementChild!.children[1]);
  const expected = getComputedStyle(reference.lastElementChild!.children[1]);
  for (const property of ['height', 'backdrop-filter', 'mask-image', '-webkit-mask-image']) {
    expect(actual.getPropertyValue(property)).toBe(expected.getPropertyValue(property));
  }
});
