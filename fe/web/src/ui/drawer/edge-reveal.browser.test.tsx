import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { Drawer, drawerSeamAround } from './public.tsx';
import '../../styles/entry.css';

afterEach(cleanup);
function pause(node: HTMLElement) {
  const animation = node.getAnimations()[0];
  expect(animation).toBeDefined();
  animation.pause(); animation.currentTime = 0;
  return animation;
}
it('keeps real Drawer body at unit scale with independent unanimated panel and seam frames', async () => {
  await page.viewport(1280, 900);
  const view = render(<div style={{ position: 'relative', width: 900, height: 700 }}><Drawer open title="Reader" onClose={() => {}}><input aria-label="Draft" /></Drawer></div>);
  const panel = view.getByRole('complementary');
  const seam = drawerSeamAround(panel)!;
  pause(panel); pause(seam);
  expect(new DOMMatrix(getComputedStyle(panel).transform).a).toBe(1);
  expect(new DOMMatrix(getComputedStyle(seam).transform).a).toBe(1);
  expect(panel.parentElement).not.toBe(seam.parentElement);
  expect(panel.parentElement!.getAnimations()).toHaveLength(0);
  expect(seam.parentElement!.getAnimations()).toHaveLength(0);
  expect(panel.parentElement!.contains(seam)).toBe(false);
});
it('revokes retained panel and seam authority in the close commit while retaining the single paint tree', async () => {
  await page.viewport(1280, 900);
  const close = vi.fn();
  const content = <input aria-label="Draft" defaultValue="kept" />;
  const view = render(<Drawer open title="Reader" onClose={close}>{content}</Drawer>);
  const panel = view.getByRole('complementary'); const seam = drawerSeamAround(panel)!;
  pause(panel).currentTime = 80; pause(seam).currentTime = 80;
  const input = view.getByRole('textbox');
  view.rerender(<Drawer open={false} title="Reader" onClose={close}>{content}</Drawer>);
  pause(panel); pause(seam);
  expect(panel.isConnected).toBe(true); expect(panel.contains(input)).toBe(true);
  expect(panel.inert).toBe(true); expect(seam.inert).toBe(true);
  expect(panel.getAttribute('aria-hidden')).toBe('true'); expect(seam.getAttribute('aria-hidden')).toBe('true');
  expect(view.queryByRole('textbox')).toBeNull();
  expect(panel.hasAttribute('data-nc-drawer')).toBe(true);
  expect(panel.hasAttribute('data-nc-drawer-open')).toBe(false);
  expect(panel.hasAttribute('data-nc-escape-layer')).toBe(false);
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  expect(close).not.toHaveBeenCalled();
});

it('gives each paired paint its own layout frame outside the panel overflow', async () => {
  await page.viewport(1280, 900);
  const view = render(<Drawer open title="Frames" onClose={() => {}}>body</Drawer>);
  const panel = view.getByRole('complementary'); const seam = drawerSeamAround(panel)!;
  expect(panel.parentElement).not.toBe(seam.parentElement);
  expect(panel.parentElement!.contains(seam)).toBe(false);
  expect(panel.parentElement!.hasAttribute('data-nc-drawer-frame')).toBe(true);
  expect(seam.parentElement!.hasAttribute('data-nc-drawer-frame')).toBe(true);
});

it('lets trusted pointer and keyboard input reach the page rather than retained panel or seam controls', async () => {
  await page.viewport(1280, 900);
  const panelAction = vi.fn(); const seamAction = vi.fn(); const pageAction = vi.fn();
  const view = render(<Drawer open title="Input" onClose={() => {}}><input aria-label="Held input" defaultValue="retained" /><button type="button" onClick={panelAction}>Retained action</button></Drawer>);
  const panel = view.getByRole('complementary'); const seam = drawerSeamAround(panel)!;
  const seamButton = seam.appendChild(document.createElement('button'));
  seamButton.textContent = 'Seam action'; seamButton.addEventListener('click', seamAction);
  const input = view.getByRole('textbox') as HTMLInputElement;
  pause(panel).currentTime = 80; pause(seam).currentTime = 80; input.focus();
  view.rerender(<Drawer open={false} title="Input" onClose={() => {}}>{null}</Drawer>);
  pause(panel); pause(seam);
  await userEvent.keyboard('should not type'); expect(input.value).toBe('retained');
  for (const target of [view.getByText('Retained action'), seamButton]) {
    const box = target.getBoundingClientRect();
    const behind = document.createElement('button'); behind.textContent = 'Underlying page action';
    behind.style.cssText = `position:fixed;left:${box.left}px;top:${box.top}px;width:${box.width}px;height:${box.height}px`;
    behind.addEventListener('click', pageAction); document.body.append(behind);
    try { await page.getByRole('button', { name: 'Underlying page action' }).click(); } finally { behind.remove(); }
  }
  expect(pageAction).toHaveBeenCalledTimes(2); expect(panelAction).not.toHaveBeenCalled(); expect(seamAction).not.toHaveBeenCalled();
});

it('observes primary and peer layout resizing through the existing paint observers without a global event', async () => {
  await page.viewport(1280, 900);
  const view = render(<Drawer open title="Resize" onClose={() => {}}>body</Drawer>);
  const panel = view.getByRole('complementary'); const seam = drawerSeamAround(panel)!;
  const nodes = [panel, seam];
  for (const changed of nodes) {
    for (const node of nodes) { const animation = pause(node); animation.currentTime = 80; }
    const before = nodes.map(node => new DOMMatrix(getComputedStyle(node).transform));
    const old = panel.getAnimations()[0];
    changed.closest<HTMLElement>('[data-nc-drawer-frame]')!.style.inlineSize = changed === panel ? '480.5px' : '30.25px';
    await expect.poll(() => panel.getAnimations()[0]).not.toBe(old);
    for (const [i, node] of nodes.entries()) {
      pause(node);
      const matrix = new DOMMatrix(getComputedStyle(node).transform);
      expect(matrix.e).toBeCloseTo(before[i].e, 4); expect(matrix.f).toBeCloseTo(before[i].f, 4);
    }
  }
});

// Retained-exit fixtures must enter first; a frozen entry at its closed endpoint
// has no distance or wall-clock velocity left to animate on dismissal.
it.each(['paused0', 'zero-rate0', 'paused80'] as const)('applies the real Drawer exit boundary at %s', async mode => {
  await page.viewport(1280, 900);
  const content = <input aria-label="Boundary draft" />;
  const view = render(<Drawer open title="Boundary" onClose={() => {}}>{content}</Drawer>);
  const panel = view.getByRole('complementary'); const seam = drawerSeamAround(panel)!;
  const frames = [panel.parentElement!, seam.parentElement!];
  const entering = [panel, seam].map(node => node.getAnimations()[0]);
  for (const animation of entering) {
    expect(animation).toBeDefined();
    if (mode === 'zero-rate0') {
      animation.playbackRate = 0; animation.startTime = document.timeline.currentTime;
    } else animation.pause();
    animation.currentTime = mode === 'paused80' ? 80 : 0;
    expect(animation.currentTime).toBe(mode === 'paused80' ? 80 : 0);
    expect(animation.playState).toBe(mode === 'zero-rate0' ? 'running' : 'paused');
    if (mode === 'zero-rate0') expect(animation.playbackRate).toBe(0);
  }
  view.rerender(<Drawer open={false} title="Boundary" onClose={() => {}}>{content}</Drawer>);
  expect(view.queryByRole('complementary')).toBeNull();
  expect(view.queryByRole('textbox')).toBeNull();
  for (const [index, node] of [panel, seam].entries()) {
    if (mode === 'paused80') {
      expect(node.isConnected).toBe(true); expect(node.parentElement).toBe(frames[index]);
      expect(node.inert).toBe(true); expect(node.getAttribute('aria-hidden')).toBe('true');
      expect(node.getAnimations()).toHaveLength(1);
      expect(node.getAnimations()[0]).not.toBe(entering[index]);
    } else {
      expect(node.isConnected).toBe(false); expect(frames[index].isConnected).toBe(false);
      expect(node.getAnimations()).toHaveLength(0);
    }
    expect(entering[index].playState).toBe('idle');
  }
  expect(view.container.querySelectorAll('[data-nc-drawer]')).toHaveLength(mode === 'paused80' ? 1 : 0);
});
