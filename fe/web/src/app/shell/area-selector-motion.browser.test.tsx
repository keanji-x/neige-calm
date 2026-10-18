import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { AreaSelector } from './area-selector.tsx';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import type { Area } from '../../../../core/domain/area.ts';
import '../../styles/entry.css';

const areas: readonly Area[] = Object.freeze([
  { id: 'product', name: 'Product', kind: 'user', color: 'blue', sort: 0, defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0 },
  { id: 'engineering', name: 'Engineering', kind: 'user', color: 'blue', sort: 1, defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0 },
]);
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

it('opens the real AreaSelector from a stable nonpressed source and retains centered placement', async () => {
  await page.viewport(1200, 900);
  render(<><DepthFeedback /><div style={{ position: 'absolute', left: 300.25, top: 80.5, width: 400.5 }}>
    <AreaSelector areas={areas} activeArea={areas[0]} onSelectArea={() => {}} onCreateArea={() => {}} />
  </div></>);
  const trigger = screen.getByRole('button', { name: 'Switch area, Product' });
  const stable = trigger.parentElement!;
  fireEvent.pointerDown(trigger, { pointerId: 1, isPrimary: true, button: 0 });
  const press = trigger.getAnimations()[0]; press.pause(); press.currentTime = 80;
  const stableBox = stable.getBoundingClientRect(); const pressedBox = trigger.getBoundingClientRect();
  expect(pressedBox.width).toBeLessThan(stableBox.width);
  act(() => { trigger.click(); });
  const menu = screen.getByRole('menu'); const animation = menu.getAnimations()[0]; animation.pause(); animation.currentTime = 0;
  const placement = menu.parentElement!; const layout = placement.getBoundingClientRect();
  expect(layout.left + layout.width / 2).toBeCloseTo(500.5, 1);
  const m = new DOMMatrix(getComputedStyle(menu).transform);
  const scale = 800 / 808;
  expect(m.e).toBeCloseTo((1 - scale) * (stableBox.left + stableBox.width / 2 - layout.left), 3);
  expect(m.f).toBeCloseTo((1 - scale) * (stableBox.top + stableBox.height / 2 - layout.top) + 0.56, 3);
  expect(menu.getAnimations()).toHaveLength(1);
  fireEvent.pointerUp(trigger, { pointerId: 1 });
});

it('selects and restores focus immediately, makes retained paint inert, and cannot select twice', () => {
  let focused: Element | null = null;
  const select = vi.fn(() => { focused = document.activeElement; });
  render(<AreaSelector areas={areas} activeArea={areas[0]} onSelectArea={select} onCreateArea={() => {}} />);
  const trigger = screen.getByRole('button', { name: 'Switch area, Product' });
  act(() => { trigger.click(); }); const menu = screen.getByRole('menu');
  menu.getAnimations()[0].pause(); menu.getAnimations()[0].currentTime = 80;
  const target = screen.getByRole('menuitem', { name: 'Engineering' });
  fireEvent.click(target);
  expect(select).toHaveBeenCalledExactlyOnceWith('engineering'); expect(focused).toBe(trigger);
  expect(menu.isConnected).toBe(true); expect(menu.inert).toBe(true); expect(menu.getAttribute('aria-hidden')).toBe('true');
  expect(screen.queryByRole('menu')).toBeNull();
  fireEvent.click(target); fireEvent.keyDown(target, { key: 'Enter' }); expect(select).toHaveBeenCalledOnce();
});

it.each(['{Space}', '{Enter}'])('supports real %s activation, roving and Escape during native presence', async key => {
  render(<AreaSelector areas={areas} activeArea={areas[0]} onSelectArea={() => {}} onCreateArea={() => {}} />);
  const trigger = screen.getByRole('button', { name: 'Switch area, Product' }); trigger.focus();
  await userEvent.keyboard(key);
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Product' }));
  await userEvent.keyboard('{ArrowDown}');
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Engineering' }));
  await userEvent.keyboard('{Escape}'); expect(document.activeElement).toBe(trigger);
  expect(screen.queryByRole('menu')).toBeNull();
});
