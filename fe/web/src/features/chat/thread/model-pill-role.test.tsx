// @vitest-environment jsdom
// The Planner's role beside the model picker, on the new-track page and in an existing Planner conversation: Member, or an Area admin.
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { PlannerRolePill } from './model-pill.tsx';

afterEach(cleanup);

function openMenu(name: string): HTMLElement {
  fireEvent.click(screen.getByRole('button', { name }));
  return screen.getByRole('menu');
}

describe('PlannerRolePill', () => {
  it.each([['member', 'Role: Member'], ['admin', 'Role: Admin']] as const)(
    'names the chosen role (%s) on its trigger', (role, name) => {
      render(<PlannerRolePill value={role} onChange={vi.fn()} />);
      const button = screen.getByRole('button', { name });
      expect(button.textContent).toBe(name);
      expect(button.getAttribute('data-variant')).toBe('ghost');
      expect(button.querySelectorAll('svg')).toHaveLength(0);
      const value = button.querySelector('span[data-nc-tone]');
      expect(value?.textContent).toBe(name.split(': ')[1]);
      expect(value?.parentElement?.firstChild?.textContent).toBe(name.split(': ')[0] + ': ');
      expect(value?.parentElement?.hasAttribute('data-nc-tone')).toBe(false);
      expect(value?.getAttribute('data-nc-tone')).toBe('neutral');
      expect(button.getAttribute('data-nc-tone')).toBe('neutral');
    },
  );

  it('offers Member then Admin, each with its one-line description, and marks the chosen one', () => {
    render(<PlannerRolePill value="member" onChange={vi.fn()} />);
    const menu = openMenu('Role: Member');
    expect(within(menu).getAllByRole('menuitem').map((row) => row.textContent)).toEqual([
      'MemberAn ordinary Planner.Selected',
      'AdminMay add Tracks and close the ones it added.',
    ]);
    expect(within(menu).queryByRole('note')).toBeNull();
  });

  it('hands back the role picked, and nothing for the one already chosen', () => {
    const onChange = vi.fn();
    render(<PlannerRolePill value="member" onChange={onChange} />);
    fireEvent.click(within(openMenu('Role: Member')).getByRole('menuitem', { name: /^Member/ }));
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(within(openMenu('Role: Member')).getByRole('menuitem', { name: /^Admin/ }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('admin');
  });

  it('carries its reason only while disabled, and stays shut', () => {
    const reason = "This Track can't be an Area admin.";
    const describedText = (button: HTMLElement) => (button.getAttribute('aria-describedby') ?? '').split(' ')
      .map((id) => document.getElementById(id)?.textContent ?? '').join(' ');
    const { rerender } = render(<PlannerRolePill value="admin" onChange={vi.fn()} disabledReason={reason} />);
    expect(describedText(screen.getByRole('button', { name: 'Role: Admin' }))).not.toContain(reason);
    rerender(<PlannerRolePill value="admin" onChange={vi.fn()} isDisabled disabledReason={reason} />);
    const button = screen.getByRole('button', { name: 'Role: Admin' });
    expect(button.getAttribute('aria-disabled')).toBe('true');
    expect(describedText(button)).toContain(reason);
    fireEvent.click(button);
    fireEvent.keyDown(button, { key: 'ArrowDown' });
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('cannot be opened while disabled', () => {
    render(<PlannerRolePill value="admin" onChange={vi.fn()} isDisabled />);
    const button = screen.getByRole('button', { name: 'Role: Admin' });
    expect(button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true').toBe(true);
  });
});
