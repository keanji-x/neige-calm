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
  it.each([['never', 'Approvals: Never'], ['ask', 'Approvals: Ask me']] as const)(
    'names the stored mode (%s) on its trigger', (mode, name) => {
      render(<PermissionModePill mode={mode} onChange={vi.fn()} />);
      expect(screen.getByRole('button', { name }).textContent).toBe(name);
    },
  );

  it('offers both modes, marks the stored one, and says when a change applies', () => {
    render(<PermissionModePill mode="never" onChange={vi.fn()} />);
    const menu = openMenu('Approvals: Never');
    const rows = within(menu).getAllByRole('menuitem');
    expect(rows.map((row) => row.textContent)).toEqual([
      'NeverSandbox only; never asks.Selected',
      'Ask mePauses the turn to ask you.',
    ]);
    expect(within(menu).getByRole('note').textContent).toBe('Applies from the next turn.');
  });

  it('hands back the mode picked, and nothing for the one already stored', () => {
    const onChange = vi.fn();
    render(<PermissionModePill mode="never" onChange={onChange} />);
    fireEvent.click(within(openMenu('Approvals: Never')).getByRole('menuitem', { name: /^Never/ }));
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(within(openMenu('Approvals: Never')).getByRole('menuitem', { name: /^Ask me/ }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith('ask');
  });

  it('cannot be opened while disabled', () => {
    render(<PermissionModePill mode="ask" onChange={vi.fn()} isDisabled />);
    const button = screen.getByRole('button', { name: 'Approvals: Ask me' });
    expect(button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true').toBe(true);
  });
});
