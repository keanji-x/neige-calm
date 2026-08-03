import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Menu } from './public.tsx';

afterEach(cleanup);

describe('menu public accessibility contract', () => {
  it('renders the trigger and menu → none → menuitem structure', () => {
    render(<Menu items={[{ label: 'Create', onSelect: vi.fn() }]} trigger={(props) => <button {...props}>Actions</button>}/>);
    const trigger = screen.getByRole('button', { name: 'Actions' });
    expect(trigger.getAttribute('aria-haspopup')).toBe('menu');
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(trigger);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    const menu = screen.getByRole('menu');
    const item = within(menu).getByRole('menuitem', { name: 'Create' });
    expect(item.parentElement?.getAttribute('role')).toBe('none');
  });
});
