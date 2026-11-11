import '../../styles/entry.css';
import { DropdownMenu, DropdownMenuItem, DropdownMenuSubMenu } from '@astryxdesign/core/DropdownMenu';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { useState } from '../state/public.ts';

beforeEach(async () => {
  await page.viewport(1400, 900);
  vi.useFakeTimers({ toFake: ['requestAnimationFrame', 'cancelAnimationFrame'] });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

function menu() {
  return render(<DropdownMenu button={{ label: 'Actions' }}>
    <DropdownMenuItem label="First" onClick={() => {}} />
    <DropdownMenuSubMenu label="More">
      <DropdownMenuItem label="Nested action" onClick={() => {}} />
    </DropdownMenuSubMenu>
  </DropdownMenu>);
}

it.each(['pointer', 'keyboard'])('keeps the ordinary %s opening focus target', (modality) => {
  menu();
  const trigger = screen.getByRole('button', { name: 'Actions' });
  trigger.focus();
  if (modality === 'pointer') fireEvent.click(trigger, { detail: 1 });
  else fireEvent.keyDown(trigger, { key: 'ArrowDown' });

  act(() => { vi.advanceTimersToNextFrame(); });

  expect(document.activeElement).toBe(modality === 'pointer'
    ? screen.getByRole('menu', { name: 'Actions' })
    : screen.getByRole('menuitem', { name: 'First' }));
});

it.each(['pointer', 'keyboard'])('preserves %s-open keyboard navigation when the opening frame arrives late', (modality) => {
  menu();
  const trigger = screen.getByRole('button', { name: 'Actions' });
  trigger.focus();
  if (modality === 'pointer') fireEvent.click(trigger, { detail: 1 });
  else fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'First' }));
  fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
  const more = screen.getByRole('menuitem', { name: 'More' });
  expect(document.activeElement).toBe(more);

  act(() => { vi.advanceTimersToNextFrame(); });

  expect(document.activeElement).toBe(more);
  fireEvent.keyDown(document.activeElement!, { key: 'ArrowRight' });
  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Nested action' }));
});

it('preserves submenu focus after the parent opening frame arrives late', () => {
  menu();
  const trigger = screen.getByRole('button', { name: 'Actions' });
  trigger.focus();
  fireEvent.click(trigger, { detail: 1 });
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
  fireEvent.keyDown(document.activeElement!, { key: 'ArrowRight' });
  const nested = screen.getByRole('menuitem', { name: 'Nested action' });
  expect(document.activeElement).toBe(nested);

  act(() => { vi.advanceTimersToNextFrame(); });

  expect(document.activeElement).toBe(nested);
});

it('does not let a closed opening frame change a later pointer open into keyboard selection', () => {
  menu();
  const trigger = screen.getByRole('button', { name: 'Actions' });
  trigger.focus();
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
  expect(trigger.getAttribute('aria-expanded')).toBe('false');
  fireEvent.click(trigger, { detail: 1 });

  act(() => { vi.advanceTimersToNextFrame(); });

  expect(document.activeElement).toBe(screen.getByRole('menu', { name: 'Actions' }));
});

it('treats a controlled programmatic reopen as keyboard after a pointer open closes before its frame', () => {
  function ControlledMenu() {
    const [open, setOpen] = useState(false);
    return <>
      <DropdownMenu button={{ label: 'Actions' }} isMenuOpen={open} onOpenChange={setOpen}>
        <DropdownMenuItem label="First" onClick={() => {}} />
      </DropdownMenu>
      <button onClick={() => setOpen(false)}>Close programmatically</button>
      <button onClick={() => setOpen(true)}>Reopen programmatically</button>
    </>;
  }
  render(<ControlledMenu />);
  const trigger = screen.getByRole('button', { name: 'Actions' });
  trigger.focus();
  fireEvent.click(trigger, { detail: 1 });
  fireEvent.click(screen.getByRole('button', { name: 'Close programmatically' }));
  expect(trigger.getAttribute('aria-expanded')).toBe('false');
  fireEvent.click(screen.getByRole('button', { name: 'Reopen programmatically' }));

  act(() => { vi.advanceTimersToNextFrame(); });

  expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'First' }));
});

it('leaves a newer focus owner alone after the menu unmounts before its opening frame', () => {
  const outside = document.createElement('button');
  outside.textContent = 'Outside';
  document.body.append(outside);
  try {
    const view = menu();
    const trigger = screen.getByRole('button', { name: 'Actions' });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    view.unmount();
    outside.focus();

    act(() => { vi.advanceTimersToNextFrame(); });

    expect(document.activeElement).toBe(outside);
  } finally { outside.remove(); }
});
