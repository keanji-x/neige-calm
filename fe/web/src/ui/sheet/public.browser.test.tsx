import '../../styles/entry.css';
import { StrictMode, createRef } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { Sheet, type SheetProps } from './public.tsx';
import * as spring from '../motion/spring.ts';
import * as motion from './motion.ts';

// Browser ESM call-through spies: real factories, solver, playback and DOM.
vi.mock('../motion/spring.ts', { spy: true });
vi.mock('./motion.ts', { spy: true });
import { HoverPreview } from '../hover-preview/public.tsx';

const frames = async (n = 3) => { for (let i = 0; i < n; i++) await new Promise(requestAnimationFrame); };
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await commands.mouseButton('body', false); await commands.emulateReducedMotion(false); });
async function mount(overrides: Partial<SheetProps> = {}, strict = false) {
  await page.viewport(600, 1000);
  const props: SheetProps = { enabled: true, open: true, label: 'Sheet test', frame: { heightPx: 600, bottomInsetPx: 0 },
    children: <button>Panel action</button>, footer: <input aria-label="Footer draft" />,
    desktopFooterRef: createRef(), dialogRef: createRef(), panelRef: createRef(), finalFocusRef: createRef(),
    focusInput: false, resolveInitialFocus: host => host.querySelector('input'), onCloseRequest: vi.fn(), onFooterHeight: vi.fn(), ...overrides };
  const element = () => strict ? <StrictMode><Sheet {...props} /></StrictMode> : <Sheet {...props} />;
  const view = render(element());
  const dialog = document.querySelector<HTMLDialogElement>('dialog')!;
  const panel = document.querySelector<HTMLDivElement>('[data-nc-sheet-panel]')!;
  const handle = document.querySelector<HTMLElement>('[data-nc-sheet-handle]')!;
  const frame = document.querySelector<HTMLElement>('[data-nc-sheet-frame]')!;
  const footer = document.querySelector<HTMLElement>('[data-nc-sheet-footer]')!;
  const update = (next: Partial<SheetProps>) => { Object.assign(props, next); view.rerender(element()); };
  const settle = async () => { await expect.poll(() => panel.getAnimations().length).toBe(0); };
  return { ...view, props, dialog, panel, handle, frame, footer, update, settle };
}
const event = (node: HTMLElement, type: string, y: number, time: number, id = 1) => {
  const e = new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: id, isPrimary: true, pointerType: 'mouse', button: 0, buttons: 1, clientY: y });
  Object.defineProperty(e, 'timeStamp', { value: time }); act(() => { node.dispatchEvent(e); });
};
async function acquire(handle: HTMLElement) {
  let started: PointerEvent | null = null;
  const capture = (event: PointerEvent) => { started = event; };
  handle.addEventListener('pointerdown', capture, { once: true });
  await commands.mouseButton('[data-nc-sheet-handle]', true);
  const down = started! as PointerEvent | null; expect(down).not.toBeNull();
  return { y: down!.clientY, time: down!.timeStamp, id: down!.pointerId };
}
const releaseMouse = () => commands.mouseButton('[data-nc-sheet-handle]', false);


// Seed through the real handle/release path. Only the native timeline is held;
// no fixture paints the panel or reimplements the spring. The two inputs yield
// distinct cap-outside positions, including the review's -52.833374px phase.
async function capOutside(travel = -120, ms = 40) {
  const view = await mount(); await view.settle();
  const down = await acquire(view.handle);
  event(view.handle, 'pointermove', down.y + travel, down.time + ms, down.id);
  event(view.handle, 'pointerup', down.y + travel, down.time + ms + 1, down.id);
  const animation = view.panel.getAnimations()[0];
  animation.pause(); await animation.ready; animation.currentTime = 50;
  await releaseMouse();
  const q = () => view.panel.getBoundingClientRect().top - view.frame.getBoundingClientRect().top;
  expect(q()).toBeLessThan(-50);
  return { ...view, animation, q };
}

it.each([
  { travel: -120, ms: 40, direction: 1 }, { travel: -120, ms: 40, direction: -1 },
  { travel: -60, ms: 5, direction: 1 }, { travel: -60, ms: 5, direction: -1 },
])('keeps cap-outside first move continuous for $travel / $ms / $direction', async ({ travel, ms, direction }) => {
  const { panel, handle, frame, footer, update, props, settle, q } = await capOutside(travel, ms);
  const before = q(); const down = await acquire(handle);
  try {
    expect(handle.hasPointerCapture(down.id)).toBe(true);
    expect(q()).toBeCloseTo(before, 1);
    event(handle, 'pointermove', down.y + direction, down.time + 20, down.id);
    expect(q() - before).toBeCloseTo(direction > 0 ? .35 : 0, 1);
    // Upward input may retain, but cannot enlarge, the acquired overshoot.
    event(handle, 'pointermove', down.y - 2, down.time + 40, down.id);
    expect(q()).toBeCloseTo(before, 1);
    event(handle, 'pointermove', down.y + 2, down.time + 60, down.id);
    expect(q() - before).toBeCloseTo(.7, 1);
    update({ frame: { heightPx: 500, bottomInsetPx: 50 } });
    expect(frame.getBoundingClientRect().top).toBe(450);
    expect(q() - before).toBeCloseTo(.7, 1);
    event(handle, 'pointermove', down.y + 3, down.time + 80, down.id);
    expect(q() - before).toBeCloseTo(1.05, 1);
    expect(getComputedStyle(footer).transform).toBe('none');
    // Return through -48 and zero. Once inside the ordinary domain the
    // acquisition exception is retired, including on subsequent reversal.
    const normal = (-40 - before) / .35;
    event(handle, 'pointermove', down.y + normal, down.time + 500, down.id);
    expect(q()).toBeCloseTo(-40, 1);
    event(handle, 'pointermove', down.y - 20, down.time + 750, down.id);
    expect(q()).toBeCloseTo(-48, 1);
    const zero = -before / .35;
    event(handle, 'pointermove', down.y + zero + 10, down.time + 1000, down.id);
    expect(q()).toBeCloseTo(10, 1);
    event(handle, 'pointermove', down.y + zero - 10, down.time + 2000, down.id);
    expect(q()).toBeCloseTo(-3.5, 1);
    event(handle, 'pointermove', down.y - 20, down.time + 3000, down.id);
    expect(q()).toBeCloseTo(-48, 1);
    event(handle, 'pointermove', down.y - 30, down.time + 3100, down.id);
    expect(q()).toBeCloseTo(-48, 1);
    event(handle, 'pointerup', down.y - 30, down.time + 3101, down.id);
    expect(vi.mocked(spring.playSpring).mock.lastCall![3]).toBe(0);
    await settle(); expect(panel.style.transform).toBe('');
    expect(props.onCloseRequest).not.toHaveBeenCalled();
  } finally { await releaseMouse(); }
});

it.each(['pointerup', 'pointercancel'] as const)(
  'hands cap-outside painted velocity through %s and a second native regrab', async ending => {
    const { panel, handle, props, settle, q } = await capOutside();
    const before = q(); const down = await acquire(handle);
    try {
      event(handle, 'pointermove', down.y + 1, down.time + 20, down.id);
      const first = q(); expect(first - before).toBeCloseTo(.35, 1);
      event(handle, 'pointermove', down.y + 2, down.time + 40, down.id);
      const second = q(); expect(second - first).toBeCloseTo(.35, 1);
      event(handle, ending, down.y + 2, down.time + 41, down.id);
      expect(handle.hasPointerCapture(down.id)).toBe(false);
      expect(vi.mocked(spring.playSpring).mock.lastCall![3]).toBeCloseTo((second - first) * 50, 0);
      const playback = panel.getAnimations()[0]; playback.pause(); await playback.ready; playback.currentTime = 1;
      expect(q()).toBeGreaterThan(second);
      // The old release/cancel is identity-fenced before native capture ends.
      event(handle, 'pointerup', down.y + 500, down.time + 42, down.id);
      expect(panel.getAnimations()[0]).toBe(playback);
      await releaseMouse();
      const held = q(); const next = await acquire(handle);
      // Shared analytic sampling vs native interpolated paint has the same
      // subpixel acquisition budget as the existing intermediate-grab test.
      expect(Math.abs(q() - held)).toBeLessThan(1);
      const acquired = q();
      event(handle, 'pointermove', next.y + 1, next.time + 20, next.id);
      expect(q() - acquired).toBeCloseTo(.35, 1);
      event(handle, 'pointercancel', next.y + 1, next.time + 21, next.id);
      await settle(); expect(props.onCloseRequest).not.toHaveBeenCalled();
    } finally { await releaseMouse(); }
  });


it('keeps outward cap-outside intent at zero painted velocity', async () => {
  const { panel, handle, q, props, settle } = await capOutside(-60, 5);
  const down = await acquire(handle); const acquired = q();
  try {
    event(handle, 'pointermove', down.y - 1, down.time + 20, down.id);
    expect(q()).toBeCloseTo(acquired, 1);
    event(handle, 'pointermove', down.y - 200, down.time + 40, down.id);
    expect(q()).toBeCloseTo(acquired, 1);
    event(handle, 'pointerup', down.y - 200, down.time + 41, down.id);
    expect(vi.mocked(spring.playSpring).mock.lastCall![3]).toBe(0);
    const playback = panel.getAnimations()[0]; playback.pause(); await playback.ready; playback.currentTime = 1;
    expect(q()).toBeGreaterThan(acquired);
    playback.play(); await settle(); expect(props.onCloseRequest).not.toHaveBeenCalled();
  } finally { await releaseMouse(); }
});

it('preserves zero paused cap-outside velocity on a no-displacement grab', async () => {
  const { panel, handle, animation, q, props, settle } = await capOutside();
  const before = q(); animation.currentTime = 51; const after = q(); animation.currentTime = 50;
  const down = await acquire(handle);
  try {
    expect(q()).toBeCloseTo(before, 1);
    // Same-coordinate moves cannot overwrite the sample with raw intent v=0.
    event(handle, 'pointermove', down.y, down.time + 20, down.id);
    event(handle, 'pointerup', down.y, down.time + 21, down.id);
    const velocity = vi.mocked(spring.playSpring).mock.lastCall![3];
    expect(velocity).toBe(0); // Paused paint can have a nonzero trajectory slope, but no wall-clock momentum.
    const playback = panel.getAnimations()[0]; playback.pause(); await playback.ready; playback.currentTime = 1;
    expect(Math.sign(q() - before)).toBe(Math.sign(after - before));
    playback.play(); await settle();
    expect(props.onCloseRequest).not.toHaveBeenCalled();
  } finally { await releaseMouse(); }
});

it('grabs native intermediate paint against the unanimated frame', async () => {
  const { panel, frame, handle, props } = await mount();
  const animation = panel.getAnimations()[0]; expect(animation).toBeDefined();
  animation.pause(); await animation.ready; animation.currentTime = 60;
  const before = panel.getBoundingClientRect().top;
  expect(Math.abs(before - frame.getBoundingClientRect().top)).toBeGreaterThan(20);
  await acquire(handle);
  try { expect(Math.abs(panel.getBoundingClientRect().top - before)).toBeLessThan(1); expect(panel.getAnimations()).toHaveLength(0); }
  finally { await releaseMouse(); }
  expect(props.onCloseRequest).not.toHaveBeenCalled();
});

it.each([{ travel: -60, expected: -21 }, { travel: -180, expected: -48 }, { travel: 60, expected: 60 }])(
  'hands actual paint velocity to the spring for travel $travel', async ({ travel, expected }) => {
    const call = vi.mocked(spring.playSpring);
    const { panel, handle, settle } = await mount(); await settle();
    const down = await acquire(handle);
    try {
      const start = panel.getBoundingClientRect().top;
      event(handle, 'pointermove', down.y + travel / 2, down.time + 100);
      const first = panel.getBoundingClientRect().top;
      event(handle, 'pointermove', down.y + travel, down.time + 200);
      const second = panel.getBoundingClientRect().top;
      expect(second - start).toBeCloseTo(expected, 2);
      // Independent real rect/event-time derivative. In the cap case both
      // last paints are beyond cap so the physical derivative is zero.
      if (travel === -180) {
        event(handle, 'pointermove', down.y - 220, down.time + 300);
        const cap = panel.getBoundingClientRect().top;
        expect(cap).toBe(second);
        event(handle, 'pointerup', down.y - 220, down.time + 301);
        expect(call.mock.lastCall![3]).toBe(0);
      } else {
        event(handle, 'pointerup', down.y + travel, down.time + 201);
        expect(call.mock.lastCall![3]).toBeCloseTo((second - first) * 10, 1);
        const playback = panel.getAnimations()[0]; playback.pause(); await playback.ready; playback.currentTime = 1;
        const after = panel.getBoundingClientRect().top;
        expect(Math.sign(after - second)).toBe(Math.sign(second - first));
      }
    } finally { await releaseMouse(); }
  });

it.each(['pointercancel', 'lostpointercapture', 'contextmenu', 'blur', 'hidden'] as const)(
  'cancels qualified travel on %s before releasing capture and ignores stale up', async reason => {
    const { panel, handle, props, dialog, settle } = await mount(); await settle();
    const down = await acquire(handle);
    try {
      event(handle, 'pointermove', down.y + 350, down.time + 100);
      expect(panel.getBoundingClientRect().top).toBeGreaterThan(700);
      if (reason === 'blur') act(() => { window.dispatchEvent(new Event('blur')); });
      else if (reason === 'hidden') {
        vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
        act(() => { document.dispatchEvent(new Event('visibilitychange')); });
      } else if (reason === 'contextmenu') act(() => { handle.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true })); });
      else event(handle, reason, down.y + 350, down.time + 101);
      event(handle, 'pointerup', down.y + 350, down.time + 102);
      await settle();
      expect(props.onCloseRequest).not.toHaveBeenCalled(); expect(dialog.open).toBe(true);
      expect(panel.style.transform).toBe('');
    } finally { await releaseMouse(); }
  });

it.each([
  { distance: 48, ms: 20, close: false }, { distance: 60, ms: 50, close: false },
  { distance: 60, ms: 49, close: true }, { distance: -60, ms: 20, close: false },
  { distance: 240, ms: 1000, close: false }, { distance: 241, ms: 1000, close: true },
])('uses strict flick and distance policy at $distance / $ms', async ({ distance, ms, close }) => {
  const { handle, props, settle } = await mount(); await settle(); const down = await acquire(handle);
  try {
    event(handle, 'pointermove', down.y + distance, down.time + ms);
    event(handle, 'pointerup', down.y + distance, down.time + ms + 1);
    expect(props.onCloseRequest).toHaveBeenCalledTimes(close ? 1 : 0);
  } finally { await releaseMouse(); }
});

it('rejects a second pointer and leaves no-move acquisition focus and selection intact', async () => {
  const { handle, props, footer, settle } = await mount({ footer: <div contentEditable suppressContentEditableWarning>Draft text</div> });
  await settle(); const editor = footer.firstElementChild as HTMLElement; editor.focus();
  document.getSelection()!.setBaseAndExtent(editor.firstChild!, 2, editor.firstChild!, 4);
  const down = await acquire(handle);
  try {
    event(handle, 'pointermove', down.y + 400, down.time + 100, 2);
    event(handle, 'pointerup', down.y + 400, down.time + 101, 2);
    expect(document.activeElement).toBe(editor); expect(document.getSelection()!.anchorOffset).toBe(2);
    expect(props.onCloseRequest).not.toHaveBeenCalled();
  } finally { await releaseMouse(); }
});

it('rebases motion contribution through frame changes and keeps the footer unscaled', async () => {
  const { panel, handle, frame, footer, update, settle } = await mount(); await settle();
  const down = await acquire(handle);
  try {
    event(handle, 'pointermove', down.y + 100, down.time + 100);
    const q = panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
    update({ frame: { heightPx: 500, bottomInsetPx: 100 } });
    expect(panel.getBoundingClientRect().top - frame.getBoundingClientRect().top).toBe(q);
    expect(footer.getBoundingClientRect().bottom).toBe(900);
    expect(footer.closest('[data-nc-sheet-panel]')).toBeNull();
    expect(getComputedStyle(footer).transform).toBe('none');
    event(handle, 'pointermove', down.y + 110, down.time + 110);
    expect(panel.getBoundingClientRect().top - frame.getBoundingClientRect().top).toBe(q + 10);
  } finally { await releaseMouse(); }
});

it('reverses from native paint and velocity and fences the superseded exit', async () => {
  const calls = vi.mocked(spring.playSpring);
  const opener = document.body.appendChild(document.createElement('button')); const focus = vi.spyOn(opener, 'focus');
  const finalFocusRef = { current: opener };
  const { panel, dialog, update, settle } = await mount({ finalFocusRef }); await settle();
  try {
    const controller = vi.mocked(motion.createSheetMotion).mock.results.at(-1)!.value as ReturnType<typeof motion.createSheetMotion>;
    const moved = vi.spyOn(controller, 'move');
    update({ open: false });
    const oldHostCompletion = moved.mock.lastCall![2];
    const exit = panel.getAnimations()[0]; exit.pause(); await exit.ready;
    // Resume at rate 1 before handoff: paused wall-clock velocity belongs to
    // the pending shared sample contract and must not be inferred locally.
    exit.play(); await exit.ready; exit.currentTime = 50; const before = panel.getBoundingClientRect().top;
    exit.currentTime = 51; const after = panel.getBoundingClientRect().top;
    exit.currentTime = 50;
    const oldFinished = exit.finished.catch(() => {});
    update({ open: true });
    expect(Math.abs(panel.getBoundingClientRect().top - before)).toBeLessThan(1);
    expect(calls.mock.lastCall![3]).toBeGreaterThan(0);
    expect(Math.abs(calls.mock.lastCall![3] - (after - before) * 1000)).toBeLessThan(150);
    const entry = panel.getAnimations()[0]; entry.pause(); await entry.ready; entry.currentTime = 1;
    expect(panel.getBoundingClientRect().top).toBeGreaterThan(before);
    await oldFinished; act(() => { oldHostCompletion(); }); await frames(); expect(dialog.open).toBe(true); expect(focus).not.toHaveBeenCalled();
    entry.finish(); await settle();
    update({ open: false }); await expect.poll(() => dialog.open).toBe(false);
    expect(focus).toHaveBeenCalledTimes(1);
  } finally { opener.remove(); }
});

it('cleans grabbed and settling lifetimes and reduces synchronously, including StrictMode', async () => {
  const first = await mount({}, true); await first.settle();
  const down = await acquire(first.handle); event(first.handle, 'pointermove', down.y + 100, down.time + 100);
  const dialog = first.dialog; first.unmount(); expect(dialog.open).toBe(false);
  await commands.mouseButton('body', false);
  const second = await mount(); const old = second.panel.getAnimations()[0];
  second.unmount(); await frames(); expect(old.playState).toBe('idle');
  await commands.emulateReducedMotion(true);
  const third = await mount(); expect(third.panel.getAnimations()).toHaveLength(0);
  third.update({ open: false }); expect(third.dialog.open).toBe(false);
});

it('tracks native pause seek resume for a panel preview while the footer stays stationary', async () => {
  const opens: (() => void)[] = [];
  const { panel, footer, update, settle } = await mount({
    children: <HoverPreview title="Panel" trigger={open => { opens[0] = open; return <button>Panel preview</button>; }}>Panel details</HoverPreview>,
    footer: <div style={{ marginLeft: 1100 }}><HoverPreview title="Footer" trigger={open => { opens[1] = open; return <button>Footer preview</button>; }}>Footer details</HoverPreview></div>,
  });
  await settle(); await page.viewport(1440, 1000);
  const handle = document.querySelector<HTMLElement>('[data-nc-sheet-handle]')!; const down = await acquire(handle);
  for (const open of opens) { act(open); await frames(8); }
  const moving = await page.getByRole('dialog', { name: 'Preview: Panel' }).findElement();
  const stationary = await page.getByRole('dialog', { name: 'Preview: Footer' }).findElement();
  await expect.poll(() => moving.getAnimations().length + stationary.getAnimations().length).toBe(0);
  const baseline = moving.getBoundingClientRect().top; const footerTop = stationary.getBoundingClientRect().top;
  try {
    event(handle, 'pointermove', down.y + 60, down.time + 100);
    await frames(8); expect(moving.getBoundingClientRect().top).not.toBe(baseline);
    expect(stationary.getBoundingClientRect().top).toBe(footerTop);
    event(handle, 'pointerup', down.y + 60, down.time + 101);
    const animation = panel.getAnimations()[0]; animation.pause(); await animation.ready; animation.currentTime = 40; await frames(8);
    await expect.poll(() => moving.getAnimations().length).toBe(0);
    const held = moving.getBoundingClientRect().top; await frames(8); expect(moving.getBoundingClientRect().top).toBe(held);
    animation.currentTime = 80; await frames(8); expect(moving.getBoundingClientRect().top).not.toBe(held);
    expect(stationary.getBoundingClientRect().top).toBe(footerTop);
    animation.play(); await settle(); await frames(8);
    update({ open: false }); update({ open: true }); await frames(8);
    expect(moving.hasAttribute('data-nc-link-preview')).toBe(false);
    expect(footer.inert).toBe(false);
  } finally { await releaseMouse(); }
});

it('preserves native q/v on a running frame rebase and restores focus only once', async () => {
  const calls = vi.mocked(spring.playSpring);
  const opener = document.body.appendChild(document.createElement('button')); const focus = vi.spyOn(opener, 'focus');
  const { panel, frame, update, settle, dialog } = await mount({ finalFocusRef: { current: opener } }); await settle();
  try {
    update({ open: false }); const exit = panel.getAnimations()[0]; exit.pause(); await exit.ready;
    exit.play(); await exit.ready; exit.currentTime = 40;
    const q = panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
    // Reopen to declare a new live frame while retaining exit velocity.
    update({ open: true, frame: { heightPx: 500, bottomInsetPx: 100 } });
    expect(panel.getBoundingClientRect().top - frame.getBoundingClientRect().top).toBeCloseTo(q, 0);
    expect(calls.mock.lastCall![3]).toBeGreaterThan(0);
    await settle(); update({ open: false }); await expect.poll(() => dialog.open).toBe(false);
    expect(focus).toHaveBeenCalledTimes(1);
    update({ frame: { heightPx: 450, bottomInsetPx: 100 } }); await frames(); expect(focus).toHaveBeenCalledTimes(1);
  } finally { opener.remove(); }
});

it('revokes delayed focusInput after a user chooses another control', async () => {
  const { update } = await mount({ focusInput: true, footer: null });
  const other = page.getByRole('button', { name: 'Panel action' }).element() as HTMLElement;
  other.focus();
  update({ footer: <input aria-label="Late editor" /> }); await frames();
  expect(document.activeElement).toBe(other);
});

it('settles runtime reduced motion and stops all idle visual work', async () => {
  const { panel, dialog, update, settle } = await mount(); await settle();
  update({ open: false }); expect(panel.getAnimations().length).toBe(1);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => dialog.open).toBe(false); expect(panel.getAnimations()).toHaveLength(0);
  update({ open: true }); expect(dialog.open).toBe(true); expect(panel.getAnimations()).toHaveLength(0);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(10);
  expect(request).toHaveBeenCalledTimes(10);
});

it('declines nonprimary and non-left admission without disturbing native paint', async () => {
  const { panel, handle, props } = await mount();
  const animation = panel.getAnimations()[0]; animation.pause(); await animation.ready; animation.currentTime = 40;
  const top = panel.getBoundingClientRect().top;
  for (const input of [{ isPrimary: false, button: 0 }, { isPrimary: true, button: 2 }]) {
    act(() => { handle.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 8, clientY: 500, ...input })); });
    expect(panel.getAnimations()[0]).toBe(animation); expect(panel.getBoundingClientRect().top).toBe(top);
  }
  expect(props.onCloseRequest).not.toHaveBeenCalled();
});

it('keeps the background interactive without outside dismissal or a focus trap', async () => {
  const background = document.body.appendChild(document.createElement('button'));
  background.textContent = 'Background action'; Object.assign(background.style, { position: 'fixed', top: '20px', left: '20px' });
  const click = vi.fn(); background.addEventListener('click', click);
  try {
    const { props, dialog, settle } = await mount(); await settle();
    await page.elementLocator(background).click(); expect(click).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(background); expect(dialog.open).toBe(true);
    expect(props.onCloseRequest).not.toHaveBeenCalled(); expect(background.closest('[inert]')).toBeNull();
  } finally { background.remove(); }
});

it('uses fractional border-box travel without rounding the declared frame', async () => {
  const calls = vi.mocked(spring.playSpring);
  const { panel, frame, update, settle } = await mount({ frame: { heightPx: 600.375, bottomInsetPx: 0 } });
  expect(frame.getBoundingClientRect().height).toBe(600.375);
  const height = panel.getBoundingClientRect().height;
  expect(height).toBe(648.375);
  expect(calls.mock.lastCall![1]).toBe(height);
  await settle(); // Closing from the entered surface, rather than the already-closed entry endpoint.
  update({ open: false }); expect(calls.mock.lastCall![2]).toBe(height);
});

it('notifies panel and footer dependencies when the layout frame moves during a lease', async () => {
  await commands.emulateReducedMotion(true);
  const opens: (() => void)[] = [];
  const { handle, update, settle } = await mount({
    children: <HoverPreview title="Frame panel" trigger={open => { opens[0] = open; return <button>Frame panel preview</button>; }}>Panel details</HoverPreview>,
    footer: <div style={{ marginLeft: 1100, height: 700 }}><HoverPreview title="Frame footer" trigger={open => { opens[1] = open; return <button>Frame footer preview</button>; }}>Footer details</HoverPreview></div>,
  });
  await page.viewport(1440, 1000); await settle(); const down = await acquire(handle);
  try {
    for (const open of opens) { act(open); await frames(5); }
    event(handle, 'pointermove', down.y + 20, down.time + 100); await frames(5);
    const panel = await page.getByRole('dialog', { name: 'Preview: Frame panel' }).findElement();
    const footer = await page.getByRole('dialog', { name: 'Preview: Frame footer' }).findElement();
    const before = [panel.getBoundingClientRect().top, footer.getBoundingClientRect().top];
    const anchors = [page.getByRole('button', { name: 'Frame panel preview' }).element(), page.getByRole('button', { name: 'Frame footer preview' }).element()];
    const sources = anchors.map(node => node.getBoundingClientRect().top);
    update({ frame: { heightPx: 600, bottomInsetPx: 100 } }); await frames(5);
    expect(anchors.map((node, i) => node.getBoundingClientRect().top - sources[i])).toEqual([-100, -100]);
    expect(panel.getBoundingClientRect().top - before[0]).toBe(-100);
    expect(footer.getBoundingClientRect().top - before[1]).toBe(-100);
  } finally { await releaseMouse(); }
});

async function focusExit() {
  const controls = document.body.appendChild(document.createElement('div'));
  const fallback = controls.appendChild(document.createElement('button'));
  fallback.textContent = 'Default focus';
  const external = controls.appendChild(document.createElement('button'));
  external.textContent = 'Chosen focus';
  const view = await mount({ finalFocusRef: { current: fallback } }); await view.settle();
  const editor = view.footer.firstElementChild as HTMLElement;
  editor.focus(); expect(document.activeElement).toBe(editor);
  return { ...view, controls, fallback, external, editor };
}

it.each([false, true])('latches external exit choice across removal and frame rebase: removed=%s', async removed => {
  const { controls, fallback, external, dialog, panel, update } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false }); expect(dialog.open).toBe(true); expect(panel.inert).toBe(true);
    external.focus(); expect(document.activeElement).toBe(external); expect(dialog.open).toBe(true);
    if (removed) external.remove();
    update({ frame: { heightPx: 550, bottomInsetPx: 20 } });
    await expect.poll(() => dialog.open).toBe(false);
    expect(focus).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(removed ? document.body : external);
    expect(panel.textContent).toBe('');
  } finally { controls.remove(); }
});

it('gives each close independent default authority after rapid reopen and stale focus events', async () => {
  const { controls, fallback, external, dialog, panel, update, settle, editor } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false }); const old = panel.getAnimations()[0]; const finished = old.finished.catch(() => {});
    external.focus(); expect(document.activeElement).toBe(external);
    update({ open: true }); editor.focus(); expect(document.activeElement).toBe(editor);
    await finished; await settle(); expect(dialog.open).toBe(true); expect(focus).not.toHaveBeenCalled();
    // A real choice while open belongs to neither old nor future close.
    external.focus(); editor.focus();
    update({ open: false });
    // Dispatching an old/synthetic focus event is not successful focus.
    external.dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(fallback); expect(focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
    update({ open: true }); await settle(); editor.focus();
    update({ open: false }); external.focus(); expect(document.activeElement).toBe(external);
    await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(external); expect(focus).toHaveBeenCalledTimes(1);
  } finally { controls.remove(); }
});

it('ignores body blur, retained panel/footer and unavailable external focus attempts', async () => {
  const { controls, fallback, external, dialog, panel, footer, editor, update } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false });
    // Native blur to BODY carries no new control choice.
    editor.blur();
    expect(document.activeElement).toBe(document.body); expect(dialog.open).toBe(true);
    (panel.querySelector('button') as HTMLElement).focus(); (footer.firstElementChild as HTMLElement).focus();
    external.disabled = true; external.focus(); external.disabled = false;
    controls.inert = true; external.focus(); controls.inert = false;
    external.hidden = true; external.focus(); external.hidden = false;
    expect(document.activeElement).toBe(document.body);
    external.setAttribute('aria-disabled', 'true'); external.focus();
    expect(document.activeElement).toBe(external); external.blur(); external.removeAttribute('aria-disabled');
    external.setAttribute('aria-hidden', 'true'); external.focus();
    expect(document.activeElement).toBe(external); external.blur(); external.removeAttribute('aria-hidden');
    document.body.dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(fallback); expect(focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
  } finally { controls.remove(); }
});

it('carries a revoked choice into runtime reduced exit and disposes it on unmount', async () => {
  const { controls, fallback, external, dialog, update, unmount } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false }); external.focus(); expect(document.activeElement).toBe(external);
    await commands.emulateReducedMotion(true);
    await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(external); expect(focus).not.toHaveBeenCalled();
    update({ open: true }); update({ open: false });
    expect(dialog.open).toBe(false); expect(document.activeElement).toBe(fallback);
    expect(focus).toHaveBeenCalledTimes(1);
    update({ open: true }); await commands.emulateReducedMotion(false);
    update({ open: false }); external.focus(); expect(document.activeElement).toBe(external);
    unmount(); await frames(); expect(dialog.open).toBe(false); expect(focus).toHaveBeenCalledTimes(1);
    external.focus(); expect(document.activeElement).toBe(external);
  } finally { controls.remove(); }
});

it('does not delay a zero-window reduced close to await a future choice', async () => {
  await commands.emulateReducedMotion(true);
  const { controls, fallback, external, dialog, update } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false }); expect(dialog.open).toBe(false);
    expect(document.activeElement).toBe(fallback); expect(focus).toHaveBeenCalledExactlyOnceWith({ preventScroll: true });
    external.focus(); await frames(); expect(document.activeElement).toBe(external); expect(focus).toHaveBeenCalledTimes(1);
  } finally { controls.remove(); }
});

it('revokes the exit listener on host migration without overriding an external choice', async () => {
  const { controls, fallback, external, dialog, update, settle, editor } = await focusExit();
  const destination = controls.appendChild(document.createElement('div'));
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false }); external.focus(); expect(document.activeElement).toBe(external);
    update({ enabled: false, desktopFooterRef: { current: destination } });
    expect(dialog.open).toBe(false); expect(document.activeElement).toBe(external); expect(focus).not.toHaveBeenCalled();
    update({ enabled: true, open: true }); await settle(); editor.focus(); expect(document.activeElement).toBe(editor);
    update({ open: false }); await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(fallback); expect(focus).toHaveBeenCalledTimes(1);
  } finally { controls.remove(); }
});

it('keeps close choice authority while exit waits for a usable layout frame', async () => {
  const { controls, fallback, external, dialog, update } = await focusExit();
  const focus = vi.spyOn(fallback, 'focus');
  try {
    update({ open: false, frame: { heightPx: 0, bottomInsetPx: 0 } });
    external.focus(); expect(document.activeElement).toBe(external); expect(dialog.open).toBe(true);
    update({ frame: { heightPx: 600, bottomInsetPx: 0 } });
    await expect.poll(() => dialog.open).toBe(false);
    expect(document.activeElement).toBe(external); expect(focus).not.toHaveBeenCalled();
  } finally { controls.remove(); }
});

// Formal PR3014 sampler integration; native controls and independent rect/time oracle.
async function signedMount() {
  await page.viewport(600, 1000);
  const props: SheetProps = { enabled: true, open: true, label: 'Signed Sheet', frame: { heightPx: 600, bottomInsetPx: 0 },
    children: <button>Action</button>, footer: <input aria-label="Draft" />, desktopFooterRef: createRef(), dialogRef: createRef(), panelRef: createRef(),
    finalFocusRef: createRef(), focusInput: false, resolveInitialFocus: host => host.querySelector('input'), onCloseRequest: vi.fn(), onFooterHeight: vi.fn() };
  const view = render(<Sheet {...props} />);
  const panel = document.querySelector<HTMLElement>('[data-nc-sheet-panel]')!;
  const frame = document.querySelector<HTMLElement>('[data-nc-sheet-frame]')!;
  const handle = document.querySelector<HTMLElement>('[data-nc-sheet-handle]')!;
  const dialog = document.querySelector<HTMLDialogElement>('dialog')!;
  const update = (next: Partial<SheetProps>) => { Object.assign(props, next); view.rerender(<Sheet {...props} />); };
  const q = () => panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
  return { panel, frame, handle, dialog, update, q };
}
function signedPointer(handle: HTMLElement, type: string, id: number, y: number, time: number) {
  const event = new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: id, isPrimary: true, button: 0, buttons: 1, clientY: y });
  Object.defineProperty(event, 'timeStamp', { value: time });
  act(() => { handle.dispatchEvent(event); });
}

// Native mouse admission establishes a real active pointer. Synthetic monotonic
// moves/release then seed the production spring; the mouse stays held so a
// synchronous second grab can be measured without transport/frame latency.
async function seed() {
  const view = await signedMount();
  await expect.poll(() => view.panel.getAnimations().length).toBe(0);
  let down: PointerEvent | undefined;
  view.handle.addEventListener('pointerdown', event => { down = event; }, { once: true });
  await commands.mouseButton('[data-nc-sheet-handle]', true);
  expect(down).toBeDefined();
  const { pointerId: id, clientY: y, timeStamp: time } = down!;
  signedPointer(view.handle, 'pointermove', id, y + 60, time + 100);
  signedPointer(view.handle, 'pointerup', id, y + 60, time + 101);
  const animation = view.panel.getAnimations()[0];
  expect(vi.mocked(spring.playSpring).mock.lastCall![3]).toBe(600);
  animation.pause(); await animation.ready;
  return { ...view, animation, id, y, time };
}

const modes = Object.freeze(['forward', 'negative', 'half', 'reverse', 'paused', 'zero'] as const);
const actions = Object.freeze(['grab-release', 'reopen', 'frame'] as const);
it.each(modes.flatMap(mode => actions.map(action => ({ mode, action }))))(
  'consumes signed native $mode velocity through $action', async ({ mode, action }) => {
    const view = await seed();
    const { animation, panel, handle, q, update, id, y, time } = view;
    animation.currentTime = 80;
    if (mode === 'reverse') { animation.reverse(); await animation.ready; }
    else if (mode !== 'paused') {
      animation.playbackRate = mode === 'negative' ? -1 : mode === 'half' ? .5 : mode === 'zero' ? 0 : 1;
      animation.play(); await animation.ready;
    }
    expect(animation.playState).toBe(mode === 'paused' ? 'paused' : 'running');
    // Independent native painted/time difference, not Playback.sample or a
    // trajectory copy. Interpolation and analytic derivative differ slightly.
    animation.currentTime = 79; const before = q();
    animation.currentTime = 81; const after = q();
    animation.currentTime = 80; const held = q();
    const rate = mode === 'paused' || mode === 'zero' ? 0 : mode === 'negative' || mode === 'reverse' ? -1 : mode === 'half' ? .5 : 1;
    const expected = (after - before) * 500 * rate;
    expect(Math.abs(after - before)).toBeGreaterThan(.1);
    const calls = vi.mocked(spring.playSpring); const count = calls.mock.calls.length;
    if (action === 'grab-release') {
      signedPointer(handle, 'pointerdown', id, y, time + 200);
      expect(panel.getAnimations()).toHaveLength(0);
      expect(Math.abs(q() - held)).toBeLessThan(1);
      signedPointer(handle, 'pointerup', id, y, time + 201);
    } else if (action === 'reopen') {
      update({ open: false });
      // Close has a new spring. Control that actual exit, then reopen.
      const exit = panel.getAnimations()[0]; exit.pause(); await exit.ready;
      exit.playbackRate = rate; if (mode !== 'paused') { exit.play(); await exit.ready; }
      exit.currentTime = 79; const a = q(); exit.currentTime = 81; const b = q(); exit.currentTime = 80;
      const position = q(); update({ open: true });
      expect(Math.abs(q() - position)).toBeLessThan(1);
      const inherited = calls.mock.lastCall![3];
      if (rate === 0) expect(Math.abs(inherited)).toBe(0);
      else { expect(Math.sign(inherited)).toBe(Math.sign((b - a) * rate)); expect(Math.abs(inherited - (b - a) * 500 * rate)).toBeLessThan(150); }
      return;
    } else update({ frame: { heightPx: 500, bottomInsetPx: 100 } });
    expect(calls.mock.calls.length).toBe(count + 1);
    expect(Math.abs(q() - held)).toBeLessThan(1);
    const inherited = calls.mock.lastCall![3];
    if (rate === 0) expect(Math.abs(inherited)).toBe(0);
    else { expect(Math.sign(inherited)).toBe(Math.sign(expected)); expect(Math.abs(inherited - expected)).toBeLessThan(150); }
    const next = panel.getAnimations()[0]; next.pause(); await next.ready; next.currentTime = 1;
    if (rate !== 0) expect(Math.sign(q() - held)).toBe(Math.sign(expected));
  });

it.each(['before', 'after'] as const)('consumes pending rate controls %s ready on a frame rebase', async phase => {
  const { panel, animation, q, update } = await seed();
  animation.play(); await animation.ready; animation.currentTime = 80;
  animation.updatePlaybackRate(-.5); expect(animation.pending).toBe(true);
  if (phase === 'after') await animation.ready;
  expect(animation.pending).toBe(phase === 'before');
  const rate = animation.playbackRate;
  animation.currentTime = 79; const a = q(); animation.currentTime = 81; const b = q(); animation.currentTime = 80;
  const held = q(); update({ frame: { heightPx: 550, bottomInsetPx: 20 } });
  expect(Math.abs(q() - held)).toBeLessThan(1);
  const velocity = vi.mocked(spring.playSpring).mock.lastCall![3];
  expect(Math.sign(velocity)).toBe(Math.sign((b - a) * rate));
  expect(Math.abs(velocity - (b - a) * 500 * rate)).toBeLessThan(150);
  expect(panel.getAnimations()).toHaveLength(1);
});

it.each(['before', 'after'] as const)('consumes native pause %s ready without momentum on rebase', async phase => {
  const { animation, q, update } = await seed();
  animation.play(); await animation.ready; animation.currentTime = 80;
  animation.pause(); expect(animation.pending).toBe(true);
  if (phase === 'after') await animation.ready;
  expect(animation.playState).toBe('paused');
  const held = q(); update({ frame: { heightPx: 550, bottomInsetPx: 20 } });
  expect(Math.abs(q() - held)).toBeLessThan(1);
  expect(Math.abs(vi.mocked(spring.playSpring).mock.lastCall![3])).toBe(0);
});

it.each(['running', 'paused'] as const)('holds negative native time at the explicit initial endpoint while %s', async state => {
  const { animation, q, update } = await seed();
  if (state === 'running') { animation.play(); await animation.ready; }
  animation.currentTime = -1;
  expect(q()).toBe(60);
  update({ frame: { heightPx: 500, bottomInsetPx: 100 } });
  expect(q()).toBe(60);
  expect(vi.mocked(spring.playSpring).mock.lastCall![3]).toBe(state === 'running' ? 600 : 0);
});

it.each(['paused', 'zero'] as const)('closes synchronously at the closed endpoint with %s entry and no artificial retention', async state => {
  const { panel, frame, dialog, update } = await signedMount();
  const animation = panel.getAnimations()[0]; animation.pause(); await animation.ready;
  if (state === 'zero') { animation.playbackRate = 0; animation.play(); await animation.ready; }
  animation.currentTime = -1;
  expect(panel.getBoundingClientRect().top - frame.getBoundingClientRect().top).toBe(panel.getBoundingClientRect().height);
  const count = vi.mocked(spring.playSpring).mock.calls.length;
  update({ open: false });
  expect(dialog.open).toBe(false); expect(panel.getAnimations()).toHaveLength(0);
  expect(vi.mocked(spring.playSpring).mock.calls.length).toBe(count);
});

it('settles a paused terminal endpoint on rebase without manufacturing a playback', async () => {
  const { animation, panel, q, update } = await seed();
  const duration = Number(animation.effect!.getComputedTiming().duration);
  animation.currentTime = duration + 1; expect(q()).toBe(0);
  const count = vi.mocked(spring.playSpring).mock.calls.length;
  update({ frame: { heightPx: 550, bottomInsetPx: 20 } });
  expect(q()).toBe(0); expect(panel.getAnimations()).toHaveLength(0);
  expect(vi.mocked(spring.playSpring).mock.calls.length).toBe(count);
});


it.each([false, true])('prepares first entry only after a closed native mount is shown: strict=%s', async strict => {
  const { panel, frame, dialog, update, props } = await mount({ open: false }, strict);
  await frames(2);
  expect(dialog.open).toBe(false); expect(panel.getBoundingClientRect().height).toBe(0);
  const calls = vi.mocked(spring.playSpring); const count = calls.mock.calls.length;
  const focus = vi.spyOn(HTMLElement.prototype, 'focus');
  update({ frame: { heightPx: 0, bottomInsetPx: 20 } });
  update({ frame: { heightPx: 500.375, bottomInsetPx: 50 } });
  expect(calls.mock.calls.length).toBe(count); expect(focus).not.toHaveBeenCalled();
  expect(props.onCloseRequest).not.toHaveBeenCalled();
  let captured: Animation | undefined; let initial = 0;
  const { playSpring: original } = await vi.importActual<typeof spring>('../motion/spring.ts');
  calls.mockImplementation((...args) => {
    initial = panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
    const playback = original(...args); captured = playback.animations[0]; captured.pause(); return playback;
  });
  try { update({ open: true }); } finally { calls.mockImplementation(original); }
  expect(captured).toBeDefined(); expect(dialog.open).toBe(true);
  const height = panel.getBoundingClientRect().height;
  expect(height).toBe(548.375); expect(initial).toBeCloseTo(height, 1);
  expect(calls.mock.lastCall!.slice(1, 4)).toEqual([height, 0, 0]);
  await captured!.ready;
  captured!.currentTime = Number(captured!.effect!.getComputedTiming().duration) * .15;
  const q = panel.getBoundingClientRect().top - frame.getBoundingClientRect().top;
  expect(q).toBeGreaterThan(10); expect(q).toBeLessThan(height - 10);
});

it('waits for a valid first frame and never finishes a never-presented close', async () => {
  vi.mocked(spring.playSpring).mockClear();
  const { panel, dialog, update } = await mount({ open: false, frame: { heightPx: 0, bottomInsetPx: 0 } });
  const close = vi.spyOn(dialog, 'close'); const calls = vi.mocked(spring.playSpring);
  update({ open: true }); expect(dialog.open).toBe(false); expect(calls).not.toHaveBeenCalled();
  update({ open: false }); expect(close).not.toHaveBeenCalled();
  update({ open: true, frame: { heightPx: 600, bottomInsetPx: 0 } });
  expect(dialog.open).toBe(true); expect(calls.mock.lastCall!.slice(1, 4)).toEqual([648, 0, 0]);
  expect(panel.getAnimations()).toHaveLength(1);
});


it.each(['enable', 'remount', 'hidden', 'reduced'] as const)('prepares an unavailable first presentation through %s', async mode => {
  vi.mocked(spring.playSpring).mockClear();
  let view = await mount({ open: false, enabled: mode !== 'enable' });
  if (mode === 'remount') { view.unmount(); view = await mount({ open: false }); }
  if (mode === 'enable') view.update({ enabled: true });
  const panel = document.querySelector<HTMLDivElement>('[data-nc-sheet-panel]')!;
  const dialog = document.querySelector<HTMLDialogElement>('dialog')!;
  const frame = document.querySelector<HTMLElement>('[data-nc-sheet-frame]')!;
  const close = vi.spyOn(dialog, 'close'); const calls = vi.mocked(spring.playSpring);
  if (mode === 'reduced') await commands.emulateReducedMotion(true);
  expect(calls).not.toHaveBeenCalled(); expect(close).not.toHaveBeenCalled();
  if (mode === 'hidden') panel.hidden = true;
  view.update({ open: true });
  if (mode === 'hidden') {
    expect(panel.getBoundingClientRect().height).toBe(0); expect(calls).not.toHaveBeenCalled();
    panel.hidden = false; view.update({ frame: { heightPx: 500, bottomInsetPx: 10 } });
  }
  expect(dialog.open).toBe(true);
  if (mode === 'reduced') {
    expect(calls).not.toHaveBeenCalled(); expect(panel.getBoundingClientRect().top).toBe(frame.getBoundingClientRect().top);
    view.update({ open: false }); expect(dialog.open).toBe(false); expect(panel.textContent).toBe('');
  } else {
    expect(panel.getAnimations()).toHaveLength(1);
    expect(calls.mock.lastCall!.slice(1, 4)).toEqual([panel.getBoundingClientRect().height, 0, 0]);
  }
});
