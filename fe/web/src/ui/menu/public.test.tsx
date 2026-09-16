import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Menu } from './public.tsx';

afterEach(cleanup);

describe('Menu behavior', () => {
  it('focuses the first item after opening', async () => {
    render(<Menu items={[{ label: 'Create', onSelect: vi.fn() }]} trigger={(props) => <button {...props}>Actions</button>}/>);
    fireEvent.click(screen.getByRole('button', { name: 'Actions' }));
    await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Create' }));
  });

  it('resets the active item when reopening', async () => {
    render(<Menu items={['First', 'Second', 'Third'].map((label) => ({ label, onSelect: vi.fn() }))}
      trigger={(props) => <button {...props}>Actions</button>}/>);
    const trigger = screen.getByRole('button', { name: 'Actions' });
    fireEvent.click(trigger); await Promise.resolve();
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'First' }), { key: 'ArrowDown' });
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Second' }), { key: 'ArrowDown' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Third' }));
    fireEvent.click(trigger);
    fireEvent.click(trigger); await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'First' }));
  });

  it('restores trigger focus before selection and on Escape', () => {
    let focusedDuringSelection: Element | null = null;
    render(<Menu items={[{ label: 'Create', onSelect: () => { focusedDuringSelection = document.activeElement; } }]} trigger={(props) => <button {...props}>Actions</button>}/>);
    const trigger = screen.getByRole('button', { name: 'Actions' });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: 'Create' }));
    expect(focusedDuringSelection).toBe(trigger);
    expect(screen.queryByRole('menu')).toBeNull();
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole('menuitem'), { key: 'Escape' });
    expect(document.activeElement).toBe(trigger);
  });
  it('marks only a current destination and separates actions without adding a keyboard stop', async () => {
    render(<Menu items={[
      { label: 'Product', current: true, onSelect: vi.fn() },
      { label: 'Frontend', onSelect: vi.fn() },
      { label: 'New area', separatorBefore: true, onSelect: vi.fn() },
    ]} separatorClassName="divider" trigger={(props) => <button {...props}>Areas</button>} />);
    fireEvent.click(screen.getByRole('button', { name: 'Areas' }));
    expect(screen.getByRole('menuitem', { name: 'Product' }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('menuitem', { name: 'Frontend' }).hasAttribute('aria-current')).toBe(false);
    expect(screen.getByRole('separator').className).toBe('divider');
    expect(screen.getByRole('separator').nextElementSibling?.textContent).toBe('New area');
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Product' }), { key: 'ArrowDown' });
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Frontend' }), { key: 'ArrowDown' });
    await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'New area' }));
  });

});
