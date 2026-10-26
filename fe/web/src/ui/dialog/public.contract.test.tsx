import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from './public.tsx';

afterEach(cleanup);

describe('dialog public accessibility contract', () => {
  it('names the modal dialog strictly from its title, including with a hidden title row', () => {
    const result = render(<Dialog purpose="info" origin={{ kind: 'surface-center' }} open title="Workspace picker" onClose={vi.fn()}/>);
    const dialog = screen.getByRole('dialog', { name: 'Workspace picker' });
    expect(dialog.getAttribute('aria-label')).toBe('Workspace picker');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    result.rerender(<Dialog purpose="info" origin={{ kind: 'surface-center' }} open title="Hidden heading" hideTitleRow onClose={vi.fn()}/>);
    expect(screen.getByRole('dialog', { name: 'Hidden heading' }).getAttribute('aria-label')).toBe('Hidden heading');
    expect(screen.queryByText('Hidden heading')).toBeNull();
  });
});


it.each(['form', 'info'] as const)('%s purpose controls backdrop dismissal while Close remains explicit', (purpose) => {
  const close = vi.fn();
  render(<Dialog purpose={purpose} origin={{ kind: 'surface-center' }} open title="Draft" onClose={close}><input defaultValue="Draft text" /></Dialog>);
  fireEvent.mouseDown(screen.getByRole('presentation'));
  expect(close).toHaveBeenCalledTimes(purpose === 'info' ? 1 : 0);
  fireEvent.click(screen.getByRole('button', { name: 'Close' }));
  expect(close).toHaveBeenCalledTimes(purpose === 'info' ? 2 : 1);
});
