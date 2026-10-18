import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';
import { Menu } from './public.tsx';

afterEach(cleanup);

it('selects and restores focus immediately, makes retained paint inert, and cannot select twice', () => {
  let focused: Element | null = null;
  const select = vi.fn(() => { focused = document.activeElement; });
  render(<Menu items={[{ label: 'First', onSelect: () => {} }, { label: 'Second', onSelect: select }]} trigger={props => <button {...props}>Actions</button>} />);
  const trigger = screen.getByRole('button', { name: 'Actions' });
  act(() => { trigger.click(); }); const menu = screen.getByRole('menu');
  menu.getAnimations()[0].pause(); menu.getAnimations()[0].currentTime = 80;
  const target = screen.getByRole('menuitem', { name: 'Second' });
  fireEvent.click(target);
  expect(select).toHaveBeenCalledExactlyOnceWith(); expect(focused).toBe(trigger);
  expect(menu.isConnected).toBe(true); expect(menu.inert).toBe(true); expect(menu.getAttribute('aria-hidden')).toBe('true');
  expect(screen.queryByRole('menu')).toBeNull();
  fireEvent.click(target); fireEvent.keyDown(target, { key: 'Enter' }); expect(select).toHaveBeenCalledOnce();
});

it.each(['{Space}', '{Enter}'])('supports real %s activation, roving and Escape during native presence', async key => {
  render(<Menu items={[{ label: 'First', onSelect: () => {} }, { label: 'Second', onSelect: () => {} }]} trigger={props => <button {...props}>Actions</button>} />);
  const trigger = screen.getByRole('button', { name: 'Actions' }); trigger.focus();
  await userEvent.keyboard(key);
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'First' }));
  await userEvent.keyboard('{ArrowDown}');
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Second' }));
  await userEvent.keyboard('{Escape}'); expect(document.activeElement).toBe(trigger);
  expect(screen.queryByRole('menu')).toBeNull();
});

it('lets a real underlying button receive pointer hits during retained exit paint', async () => {
  render(<><style>{'.hit-menu {position:fixed;left:300px;top:100px;width:200px;z-index:10}'}</style><Menu menuClassName="hit-menu" items={[{ label: 'Close', onSelect: () => {} }]} trigger={props => <button {...props}>Actions</button>} />
    <button data-testid="behind" style={{ position: 'fixed' }}>Behind</button></>);
  fireEvent.click(screen.getByRole('button', { name: 'Actions' }));
  const menu = screen.getByRole('menu');
  menu.getAnimations()[0].pause(); menu.getAnimations()[0].currentTime = 80;
  fireEvent.click(screen.getByRole('menuitem'));
  const closing = menu.getAnimations()[0]; closing.pause(); closing.currentTime = 20;
  const rect = menu.parentElement!.getBoundingClientRect(); const behind = screen.getByTestId('behind');
  Object.assign(behind.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`, zIndex: '0' });
  const click = vi.fn(); behind.addEventListener('click', click);
  expect(document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2)).toBe(behind);
  await userEvent.click(behind); expect(click).toHaveBeenCalledOnce(); expect(menu.isConnected).toBe(true);
});
