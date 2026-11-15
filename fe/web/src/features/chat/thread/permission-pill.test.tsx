// @vitest-environment jsdom
// #2348: a Planner conversation's approval setting, beside the model picker.
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { PermissionModePill } from './model-pill.tsx';

afterEach(cleanup);

function openMenu(name: string): HTMLElement {
  fireEvent.click(screen.getByRole('button', { name }));
  return screen.getByRole('menu');
}

describe('PermissionModePill', () => {
  it.each([['never', 'Approvals: Never'], ['ask', 'Approvals: Ask me'], ['full', 'Approvals: Full access']] as const)(
    'names the stored mode (%s) on its trigger', (mode, name) => {
      render(<PermissionModePill mode={mode} onChange={vi.fn()} />);
      const button = screen.getByRole('button', { name });
      expect(button.textContent).toBe(name);
      expect(button.getAttribute('data-variant')).toBe('ghost');
      expect(button.querySelectorAll('svg')).toHaveLength(0);
      const value = button.querySelector('span[data-nc-tone]');
      expect(value?.textContent).toBe(name.split(': ')[1]);
      expect(value?.parentElement?.firstChild?.textContent).toBe(name.split(': ')[0] + ': ');
      expect(value?.parentElement?.hasAttribute('data-nc-tone')).toBe(false);
      expect(value?.getAttribute('data-nc-tone')).toBe(mode === 'never' ? 'neutral' : mode === 'ask' ? 'warning' : 'danger');
      expect(button.getAttribute('data-nc-tone')).toBe(mode === 'never' ? 'neutral' : mode === 'ask' ? 'warning' : 'danger');
    },
  );

  it('offers every mode, marks the stored one, and says when a change applies', () => {
    render(<PermissionModePill mode="never" onChange={vi.fn()} />);
    const menu = openMenu('Approvals: Never');
    const rows = within(menu).getAllByRole('menuitem');
    expect(rows.map((row) => row.textContent)).toEqual([
      'NeverSandbox only; never asks.Selected',
      'Ask mePauses the turn to ask you.',
      'Full accessNo sandbox; never asks. Runs with your account\'s access.',
    ]);
    expect(within(menu).getByRole('menuitem', { name: /^Full access/ }).getAttribute('data-variant')).toBe('destructive');
    expect(within(menu).getByRole('note').textContent).toBe('Applies from the next turn.');
  });

  it('hands back the mode picked, and nothing for the one already stored', () => {
    const onChange = vi.fn();
    render(<PermissionModePill mode="never" onChange={onChange} />);
    fireEvent.click(within(openMenu('Approvals: Never')).getByRole('menuitem', { name: /^Never/ }));
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(within(openMenu('Approvals: Never')).getByRole('menuitem', { name: /^Ask me/ }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('ask');
    fireEvent.click(within(openMenu('Approvals: Never')).getByRole('menuitem', { name: /^Full access/ }));
    expect(onChange).toHaveBeenLastCalledWith('full');
  });

  it('cannot be opened while disabled', () => {
    render(<PermissionModePill mode="ask" onChange={vi.fn()} isDisabled />);
    const button = screen.getByRole('button', { name: 'Approvals: Ask me' });
    expect(button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true').toBe(true);
  });
});
