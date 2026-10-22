import '../../styles/entry.css';
import { useState } from '../../ui/state/public.ts';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import * as spring from '../../ui/motion/spring.ts';
vi.mock('../../ui/motion/spring.ts', { spy: true });
import { commands, page, userEvent } from 'vitest/browser';
import { ConversationSurface } from './mobile-conversation-surface.tsx';

// These behavioral baselines were recorded through the old public panel ref
// and real handle before replacing the vendor entry.
const handleSelector = '[data-nc-mobile-chat-panel] > [aria-hidden="true"]:first-child';
const frames = async (count: number) => {
  for (let index = 0; index < count; index++) await new Promise(requestAnimationFrame);
};
function Fixture() {
  const [open, setOpen] = useState(true);
  return <ConversationSurface mobileSheet open={open} title="Motion handoff" onClose={() => setOpen(false)}
    footer={<input aria-label="Motion draft" />}><p>Real conversation surface</p></ConversationSurface>;
}
async function mount() {
  await page.viewport(390, 844);
  render(<Fixture />);
  const dialog = await page.getByRole('dialog', { name: 'Motion handoff' }).findElement() as HTMLDialogElement;
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  const handle = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel] > [aria-hidden="true"]:first-child')!;
  return { dialog, panel, handle };
}
// Observe only this fixture's real panel playback while its layout effect is
// still on the stack. The spy always returns the original Playback unchanged.
async function renderEntry(freeze: boolean) {
  const calls = vi.mocked(spring.playSpring);
  const { playSpring: original } = await vi.importActual<typeof spring>('../../ui/motion/spring.ts');
  let count = 0;
  let captured: { panel: HTMLElement; playback: spring.SpringPlayback; entry: Animation } | undefined;
  calls.mockImplementation((...args) => {
    const playback = original(...args);
    const panel = args[0][0];
    if (panel instanceof HTMLElement && panel.matches('[data-nc-mobile-chat-panel]')) {
      count++;
      const entry = playback.animations[0];
      captured = { panel, playback, entry };
      if (freeze) entry.pause();
    }
    return playback;
  });
  try { render(<Fixture />); } finally { calls.mockImplementation(original); }
  expect(count, 'one production entry').toBe(1);
  expect(captured, 'real production entry playback captured synchronously').toBeDefined();
  return captured!;
}
afterEach(async () => {
  try { cleanup(); } finally { vi.restoreAllMocks(); await page.viewport(1280, 800); }
});

it('captures the same native entry across ready while deferred lookup can miss completed paint', async () => {
  await page.viewport(390, 844);
  const expired = await renderEntry(false);
  expect(expired.panel.getAnimations()).toContain(expired.entry);
  expired.entry.finish(); // Fixed schedule: completion precedes the old async observation.
  await expired.playback.finished;
  await page.getByRole('dialog', { name: 'Motion handoff' }).findElement();
  await frames(2);
  expect(expired.panel.getAnimations()[0]).toBeUndefined();
  cleanup();

  const { panel, playback, entry } = await renderEntry(true);
  await page.getByRole('dialog', { name: 'Motion handoff' }).findElement();
  await frames(2);
  await entry.ready;
  expect(playback.animations[0]).toBe(entry);
  expect(entry).toBeInstanceOf(Animation);
  expect((entry.effect as KeyframeEffect).target).toBe(panel);
  expect(panel.getAnimations()).toContain(entry);
  expect(entry.playState).toBe('paused');
  expect(entry.pending).toBe(false);
});

it.each([0.15, 0.3])('grabs the painted conversation sheet during entry without a position jump at phase %s', async phase => {
  await page.viewport(390, 844);
  const { panel, entry } = await renderEntry(true);
  await page.getByRole('dialog', { name: 'Motion handoff' }).findElement();
  await entry.ready;
  expect(panel.getAnimations()).toContain(entry);
  const duration = Number(entry.effect!.getComputedTiming().duration);
  expect(duration).toBeGreaterThan(0);
  try {
    entry.currentTime = duration * phase;
    const before = panel.getBoundingClientRect().top;
    // The viewport is an independent fixed ruler; a nonterminal phase must
    // actually paint away from its open layout position before acquisition.
    expect(Math.abs(before - (844 - 844 * 0.8))).toBeGreaterThan(10);
    await commands.mouseButton(handleSelector, true);
    await frames(1);
    expect(Math.abs(panel.getBoundingClientRect().top - before)).toBeLessThan(1);
    await commands.mouseButton(handleSelector, false);
  } finally { await commands.mouseButton(handleSelector, false); }
});

it('cancels a dismiss-qualified handle drag without closing the real conversation', async () => {
  const { dialog, panel, handle } = await mount();
  await expect.poll(() => panel.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  await commands.mouseButton(handleSelector, true);
  try {
    const origin = handle.getBoundingClientRect().top + handle.getBoundingClientRect().height / 2;
    const moved = new PointerEvent('pointermove', { bubbles: true, pointerId: 1, isPrimary: true,
      pointerType: 'mouse', buttons: 1, clientX: 195, clientY: origin + 360 });
    // Explicit monotonic event times pin a qualified flick independently of
    // host scheduling. These moves/cancel are synthetic, acquisition is native.
    Object.defineProperty(moved, 'timeStamp', { value: performance.now() + 100 });
    act(() => { handle.dispatchEvent(moved); });
    expect(panel.getBoundingClientRect().top).toBeGreaterThan(400);
    act(() => { fireEvent.pointerCancel(handle, { pointerId: 1, isPrimary: true, pointerType: 'mouse', clientX: 195, clientY: origin + 360 }); });
    await frames(2);
    for (const animation of dialog.getAnimations({ subtree: true })) animation.finish();
    await frames(3);
    // The controlled real caller accepts onClose. A cancellation must leave
    // its native host open and its content interactive after settling.
    expect(dialog.open).toBe(true);
    expect(panel.closest('[inert]')).toBeNull();
    act(() => { fireEvent.pointerUp(handle, { pointerId: 1, isPrimary: true, pointerType: 'mouse', clientY: origin + 360 }); });
    expect(dialog.open).toBe(true);
  } finally { await commands.mouseButton(handleSelector, false); }
});

// Real public assembly: app chooses the default dock, Sheet owns exit timing.
function FocusFixture() {
  const [open, setOpen] = useState(true);
  return <><button data-nc-chat-dock="">Default destination</button><button>External destination</button>
    <ConversationSurface mobileSheet open={open} title="Exit focus" onClose={() => setOpen(false)}
      footer={<input aria-label="Exit draft" />}><p>Retained conversation</p></ConversationSurface></>;
}
it.each([
  { width: 320, close: 'button' }, { width: 390, close: 'button' },
  { width: 320, close: 'Escape' }, { width: 390, close: 'Escape' },
])('keeps explicit external focus after conversation exit at $width via $close', async ({ width, close }) => {
  await page.viewport(width, 844); render(<FocusFixture />);
  const dialog = page.getByRole('dialog', { name: 'Exit focus' }).element() as HTMLDialogElement;
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().length).toBe(0);
  const draft = page.getByRole('textbox', { name: 'Exit draft' }).element() as HTMLElement;
  draft.focus(); expect(document.activeElement).toBe(draft);
  if (close === 'button') await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  else await userEvent.keyboard('{Escape}');
  expect(dialog.open).toBe(true); expect(panel.inert).toBe(true);
  expect(panel.getAnimations().some(animation => animation.playState === 'running')).toBe(true);
  const external = page.getByRole('button', { name: 'External destination' }).element() as HTMLElement;
  external.focus({ preventScroll: true });
  expect(document.activeElement).toBe(external); expect(dialog.open).toBe(true);
  await expect.poll(() => dialog.open).toBe(false);
  expect(document.activeElement).toBe(external);
  expect(panel.textContent).not.toContain('Retained conversation');
});
it('restores the default conversation destination without an external choice', async () => {
  await page.viewport(390, 844); render(<FocusFixture />);
  const dialog = page.getByRole('dialog', { name: 'Exit focus' }).element() as HTMLDialogElement;
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().length).toBe(0);
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.poll(() => dialog.open).toBe(false);
  expect(document.activeElement).toBe(page.getByRole('button', { name: 'Default destination' }).element());
});

// The actual ConversationSurface changes its declared frame through its public
// fullscreen control. No test fixture supplies a sample or spring equation.
it.each(['forward', 'negative', 'half', 'reverse', 'paused', 'zero'] as const)(
  'hands signed native %s momentum through the production fullscreen frame', async mode => {
    const { panel, dialog } = await mount();
    const frame = panel.closest('[data-nc-sheet-frame]')!;
    const q = () => panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
    const animation = panel.getAnimations()[0]; animation.pause(); await animation.ready;
    animation.currentTime = 80;
    if (mode === 'reverse') { animation.reverse(); await animation.ready; }
    else if (mode !== 'paused') {
      animation.playbackRate = mode === 'negative' ? -1 : mode === 'half' ? .5 : mode === 'zero' ? 0 : 1;
      animation.play(); await animation.ready;
    }
    animation.currentTime = 79; const a = q(); animation.currentTime = 81; const b = q(); animation.currentTime = 80;
    const held = q(); expect(Math.abs(b - a)).toBeGreaterThan(.1);
    const rate = mode === 'paused' || mode === 'zero' ? 0 : mode === 'negative' || mode === 'reverse' ? -1 : mode === 'half' ? .5 : 1;
    const calls = vi.mocked(spring.playSpring); const count = calls.mock.calls.length;
    act(() => { fireEvent.click(page.getByRole('button', { name: 'Expand conversation' }).element()); });
    expect(calls.mock.calls.length).toBe(count + 1);
    expect(Math.abs(q() - held)).toBeLessThan(1);
    const velocity = calls.mock.lastCall![3];
    if (rate === 0) expect(Math.abs(velocity)).toBe(0);
    else {
      expect(Math.sign(velocity)).toBe(Math.sign((b - a) * rate));
      expect(Math.abs(velocity - (b - a) * 500 * rate)).toBeLessThan(150);
      const next = panel.getAnimations()[0]; next.pause(); await next.ready; next.currentTime = 1;
      expect(Math.sign(q() - held)).toBe(Math.sign(velocity));
    }
    expect(dialog.open).toBe(true); expect(panel.closest('[inert]')).toBeNull();
  });
