import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { Button } from '@astryxdesign/core';
import { createPortal } from 'react-dom';
import { DepthFeedback } from './depth-feedback.tsx';
import { pressDepthFrame, presenceDepthFrame, TAP_VELOCITY } from './depth.ts';
import { springTrajectory } from './spring.ts';
import { Drawer, drawerSeamAround } from '../drawer/public.tsx';
import { ConfirmDialog } from '../dialog/public.tsx';
import '../../styles/entry.css';

declare module 'vitest/browser' {
  interface BrowserCommands { mouseButton(selector: string, pressed: boolean): Promise<void> }
}

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
const down = (element: HTMLElement) => fireEvent.pointerDown(element, { pointerId: 1, isPrimary: true, button: 0 });
const up = (element: HTMLElement) => fireEvent.pointerUp(element, { pointerId: 1 });
const frames = (animation: Animation) => (animation.effect as KeyframeEffect).getKeyframes();

it('presses the complete native, Astryx and portal control without replacing layout transforms', () => {
  render(<><DepthFeedback /><button style={{ transform: 'translateX(17px)', translate: '3px 0' }}><span>Native</span></button>
    <Button label="Astryx">Astryx</Button>{createPortal(<button>Portal</button>, document.body)}</>);
  for (const name of ['Native', 'Astryx', 'Portal']) {
    const button = screen.getByRole('button', { name });
    down(button.querySelector('span') ?? button);
    expect(button.getAnimations()).toHaveLength(1);
    const animation = button.getAnimations()[0];
    animation.pause(); animation.currentTime = 80;
    expect(frames(animation).every(frame => frame.composite === 'add')).toBe(true);
    expect(button.querySelector('span')?.getAnimations() ?? []).toHaveLength(0);
    if (name === 'Native') {
      const matrix = new DOMMatrix(getComputedStyle(button).transform);
      expect(matrix.a).toBeLessThan(1);
      expect(matrix.e).toBeCloseTo(17, 4);
      expect(matrix.f).toBeGreaterThan(0);
      expect(getComputedStyle(button).translate).toBe('3px');
    }
    up(button);
    expect(button.getAnimations()).toHaveLength(1);
  }
});

it('shows an instantaneous tap, inherits analytical velocity on reversal and releases owned effects', async () => {
  const click = vi.fn();
  render(<><DepthFeedback /><button onClick={click}>Tap</button></>);
  const button = screen.getByRole('button', { name: 'Tap' });
  down(button);
  const inward = button.getAnimations()[0];
  inward.startTime = document.timeline.currentTime; inward.currentTime = 0;
  up(button); button.click();
  expect(click).toHaveBeenCalledOnce();
  const release = button.getAnimations()[0];
  expect(release).not.toBe(inward);
  expect(frames(release)[0].transform).toBe(pressDepthFrame(0).transform);
  const trajectory = springTrajectory(0, 0, TAP_VELOCITY);
  expect((release.effect as KeyframeEffect).getTiming().duration).toBe(trajectory.duration);
  expect(frames(release).some(frame => new DOMMatrix(frame.transform as string).a < .99)).toBe(true);
  release.startTime = document.timeline.currentTime; release.currentTime = 30;
  const state = trajectory.sample(30);
  down(button);
  const reverse = button.getAnimations()[0];
  expect(new DOMMatrix(frames(reverse)[0].transform as string).a).toBeCloseTo(new DOMMatrix(pressDepthFrame(state.value).transform as string).a, 5);
  const expected = springTrajectory(state.value, 1, state.velocity);
  expect((reverse.effect as KeyframeEffect).getTiming().duration).toBe(expected.duration);
  up(button);
  await expect.poll(() => button.getAnimations().length).toBe(0);
  expect(button.style.transform).toBe('');
});

it('preserves native focus and Space/Enter click semantics without waiting for feedback', async () => {
  const click = vi.fn();
  render(<><DepthFeedback /><Button label="Keyboard" onClick={click}>Keyboard</Button></>);
  const button = screen.getByRole('button', { name: 'Keyboard' });
  button.focus();
  fireEvent.keyDown(button, { key: ' ' });
  const first = button.getAnimations()[0];
  fireEvent.keyDown(button, { key: ' ', repeat: true });
  expect(button.getAnimations()[0]).toBe(first);
  fireEvent.keyUp(button, { key: ' ' });
  await userEvent.keyboard(' ');
  await userEvent.keyboard('{Enter}');
  expect(click).toHaveBeenCalledTimes(2);
  expect(document.activeElement).toBe(button);
});

it.each(['pointercancel', 'pointerout', 'lostpointercapture'])('releases on %s and ignores nonprimary presses', (type) => {
  render(<><DepthFeedback /><button>Cancel</button></>);
  const button = screen.getByRole('button', { name: 'Cancel' });
  fireEvent.pointerDown(button, { pointerId: 2, isPrimary: false, button: 0 });
  expect(button.getAnimations()).toHaveLength(0);
  down(button);
  const first = button.getAnimations()[0];
  button.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1 }));
  expect(first.playState).toBe('idle');
  expect(button.getAnimations()).toHaveLength(1);
});

it('yields touch pan and drag, clears blur, disabled and detached controls without observing DOM', async () => {
  const view = render(<><DepthFeedback /><button>Lifecycle</button><input aria-label="Input" /><p>Body</p><button disabled>Disabled</button><button aria-disabled="true">ARIA disabled</button></>);
  const button = screen.getByRole('button', { name: 'Lifecycle' });
  for (const element of [screen.getByRole('textbox'), screen.getByText('Body'), screen.getByText('Disabled'), screen.getByText('ARIA disabled')]) {
    down(element); expect(element.getAnimations()).toHaveLength(0);
  }
  down(button);
  const initial = button.getAnimations()[0];
  const touch = new PointerEvent('pointermove', { bubbles: true, pointerId: 1, pointerType: 'touch', clientY: 30, cancelable: true });
  expect(button.dispatchEvent(touch)).toBe(true);
  expect(initial.playState).toBe('idle');
  down(button); const drag = button.getAnimations()[0]; fireEvent.dragStart(button);
  expect(drag.playState).toBe('idle');
  down(button); fireEvent.blur(window); expect(button.getAnimations()).toHaveLength(0);
  down(button); button.setAttribute('disabled', '');
  await expect.poll(() => button.getAnimations().length).toBe(0);
  button.removeAttribute('disabled'); down(button);
  const detached = button.getAnimations()[0]; view.rerender(<DepthFeedback />);
  await expect.poll(() => detached.playState).toBe('idle');
});

it('cancels keyboard press on focus loss, reduced motion changes and unmount', async () => {
  const view = render(<><DepthFeedback /><button>Preferences</button><button>Other</button></>);
  const button = screen.getByRole('button', { name: 'Preferences' });
  button.focus(); fireEvent.keyDown(button, { key: 'Enter' });
  const first = button.getAnimations()[0]; screen.getByText('Other').focus();
  expect(first.playState).toBe('idle');
  down(button); await commands.emulateReducedMotion(true);
  await expect.poll(() => button.getAnimations().length).toBe(0);
  down(button); expect(button.getAnimations()).toHaveLength(0);
  await commands.emulateReducedMotion(false); down(button);
  const owned = button.getAnimations()[0]; view.unmount();
  expect(owned.playState).toBe('idle');
  const external = document.body.appendChild(document.createElement('button'));
  down(external); expect(external.getAnimations()).toHaveLength(0); external.remove();
});

it('uses the same restrained depth direction for real confirmation portals with immediate actions', async () => {
  const cancel = vi.fn();
  render(<><DepthFeedback /><ConfirmDialog origin={{ kind: 'surface-center' }} open title="Depth confirm" onConfirm={() => {}} onCancel={cancel} /></>);
  const dialog = screen.getByRole('dialog');
  expect(new DOMMatrix(frames(dialog.getAnimations()[0])[0].transform as string).a)
    .toBeCloseTo(new DOMMatrix(presenceDepthFrame(0).transform as string).a, 5);
  const button = screen.getByRole('button', { name: 'Cancel' });
  down(button); expect(button.getAnimations()).toHaveLength(1);
  await userEvent.click(button); expect(cancel).toHaveBeenCalledOnce();
});


it('keeps paired drawer motion continuous at unit scale on exit without a CSS translate jump', async () => {
  await page.viewport(1280, 720);
  const view = render(<div style={{ position: 'relative', height: 500, width: 700 }}>
    <Drawer open title="Depth drawer" onClose={() => {}}><p>Live text</p></Drawer></div>);
  const panel = screen.getByRole('complementary', { name: 'Depth drawer' });
  const seam = drawerSeamAround(panel)!;
  await Promise.all(panel.getAnimations().map(animation => animation.finished));
  view.rerender(<div style={{ position: 'relative', height: 500, width: 700 }}>
    <Drawer open={false} title="Depth drawer" onClose={() => {}}><p>Live text</p></Drawer></div>);
  expect(getComputedStyle(panel).translate).toBe('0px');
  expect(getComputedStyle(seam).translate).toBe('0px');
  expect(frames(panel.getAnimations()[0])).toEqual(frames(seam.getAnimations()[0]));
  for (const element of [panel, seam]) {
    const animation = element.getAnimations()[0];
    animation.pause(); animation.currentTime = 50;
    const matrix = new DOMMatrix(getComputedStyle(element).transform);
    expect(matrix.a).toBe(1);
    expect(matrix.d).toBe(1);
    expect(matrix.f).toBeGreaterThan(0);
  }
});

// Native driver events are required here: dispatchEvent does not activate CSS :active.
it.each(['native', 'astryx', 'astryx-layout'])('keeps real %s mouse paint continuous at release time zero', async (kind) => {
  render(<><DepthFeedback />{kind.startsWith('astryx')
    ? <Button label="Real press" data-testid="real-press" style={kind === 'astryx-layout' ? { transform: 'translateX(17px)' } : undefined} />
    : <button data-testid="real-press" style={{ transform: 'translateX(17px)' }}>Real press</button>}</>);
  const button = screen.getByTestId('real-press');
  try {
    await commands.mouseButton('[data-testid="real-press"]', true);
    expect(button.matches(':active')).toBe(true);
    const inward = button.getAnimations()[0];
    inward.pause(); inward.currentTime = Number((inward.effect as KeyframeEffect).getTiming().duration);
    const held = new DOMMatrix(getComputedStyle(button).transform);
    await commands.mouseButton('[data-testid="real-press"]', false);
    const release = button.getAnimations()[0];
    release.pause(); release.currentTime = 0;
    const released = new DOMMatrix(getComputedStyle(button).transform);
    expect(held.a).toBeCloseTo(800 / 828, 5);
    expect(held.e).toBeCloseTo(kind === 'astryx' ? 0 : 17, 5);
    expect(held.f).toBeCloseTo(1.96, 5);
    expect(released.a).toBeCloseTo(held.a, 5);
    expect(released.e).toBeCloseTo(held.e, 5);
    expect(released.f).toBeCloseTo(held.f, 5);
  } finally { await commands.mouseButton('[data-testid="real-press"]', false); }
});

it.each(['keyboard-first', 'pointer-first'])('holds until both sources release: %s', (order) => {
  render(<><DepthFeedback /><button>Mixed</button><button>Elsewhere</button></>);
  const button = screen.getByText('Mixed');
  button.focus();
  const keyDown = () => fireEvent.keyDown(button, { key: ' ' });
  const keyUp = () => fireEvent.keyUp(button, { key: ' ' });
  const hold = () => {
    const animation = button.getAnimations()[0];
    animation.pause(); animation.currentTime = Number((animation.effect as KeyframeEffect).getTiming().duration);
    expect(new DOMMatrix(getComputedStyle(button).transform).a).toBeCloseTo(800 / 828, 5);
  };
  if (order === 'keyboard-first') { keyDown(); down(button); up(button); }
  else { down(button); keyDown(); keyUp(); }
  hold();
  // Interleave another press/release without losing the older source.
  if (order === 'keyboard-first') { down(button); up(button); }
  else { keyDown(); screen.getByText('Elsewhere').focus(); }
  hold();
  if (order === 'keyboard-first') keyUp(); else up(button);
  const release = button.getAnimations()[0];
  release.pause(); release.currentTime = Number((release.effect as KeyframeEffect).getTiming().duration);
  expect(new DOMMatrix(getComputedStyle(button).transform).a).toBeCloseTo(1, 5);
});

it.each([['ltr', false], ['rtl', false], ['ltr', true], ['rtl', true]] as const)('keeps actual drawer/seam edges joined through entry, exit and reversal (%s, stacked=%s)', async (dir, stacked) => {
  await page.viewport(1280, 720);
  const content = (open: boolean) => <div dir={dir} style={{ position: 'relative', height: 500, width: 700 }}>
    <Drawer open={open} stacked={stacked} title="Joined" onClose={() => {}}><p>Surface</p></Drawer></div>;
  const view = render(content(true));
  const panel = screen.getByRole('complementary', { name: 'Joined' });
  const seam = drawerSeamAround(panel)!;
  const check = (time: number) => {
    for (const element of [panel, seam]) {
      const animation = element.getAnimations()[0]; animation.pause(); animation.currentTime = time;
    }
    const a = panel.getBoundingClientRect(); const b = seam.getBoundingClientRect();
    expect(dir === 'ltr' ? b.left - a.right : a.left - b.right).toBeCloseTo(0, 3);
    expect(a.top).toBeCloseTo(b.top, 3); expect(a.bottom).toBeCloseTo(b.bottom, 3);
    return [a.left, a.right, a.top, a.bottom, b.left, b.right];
  };
  check(0); check(70);
  let before = check(100);
  view.rerender(content(false));
  check(0).forEach((value, index) => expect(value).toBeCloseTo(before[index], 3));
  check(40); before = check(80);
  view.rerender(content(true));
  check(0).forEach((value, index) => expect(value).toBeCloseTo(before[index], 3));
  check(50); check(150);
});

it.each(['pointercancel', 'pointerout', 'lostpointercapture', 'dragstart', 'touch-pan'])('keeps keyboard ownership after %s, then clears the last source on focus loss', (type) => {
  render(<><DepthFeedback /><button>Owned</button><button>Next</button></>);
  const button = screen.getByText('Owned'); button.focus();
  fireEvent.keyDown(button, { key: ' ' }); down(button);
  if (type === 'touch-pan') fireEvent.pointerMove(button, { pointerId: 1, pointerType: 'touch', clientY: 30 });
  else if (type === 'dragstart') fireEvent.dragStart(button);
  else button.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1 }));
  const held = button.getAnimations()[0];
  held.pause(); held.currentTime = Number(held.effect!.getTiming().duration);
  expect(new DOMMatrix(getComputedStyle(button).transform).a).toBeCloseTo(800 / 828, 5);
  screen.getByText('Next').focus();
  const release = button.getAnimations()[0];
  expect(release).not.toBe(held);
  release.pause(); release.currentTime = Number(release.effect!.getTiming().duration);
  expect(new DOMMatrix(getComputedStyle(button).transform).a).toBeCloseTo(1, 5);
  fireEvent.keyUp(button, { key: ' ' });
  expect(button.getAnimations()[0]).toBe(release);
});

it('tracks both activation keys and independent controls through playback replacement', () => {
  render(<><DepthFeedback /><button>Keys</button><button>Pointer</button></>);
  const keys = screen.getByText('Keys'); const pointer = screen.getByText('Pointer');
  keys.focus();
  fireEvent.keyDown(keys, { key: ' ' }); fireEvent.keyDown(keys, { key: 'Enter' });
  const held = keys.getAnimations()[0];
  down(pointer); up(pointer);
  fireEvent.keyUp(keys, { key: ' ' });
  expect(keys.getAnimations()[0]).toBe(held);
  held.pause(); held.currentTime = Number(held.effect!.getTiming().duration);
  expect(new DOMMatrix(getComputedStyle(keys).transform).a).toBeCloseTo(800 / 828, 5);
  fireEvent.keyUp(keys, { key: 'Enter' });
  expect(keys.getAnimations()[0]).not.toBe(held);
});
