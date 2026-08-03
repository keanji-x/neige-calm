import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from './public.tsx';

afterEach(cleanup);

describe('dialog public accessibility contract', () => {
  it('names the modal dialog strictly from its title, including with a hidden title row', () => {
    const result = render(<Dialog open title="Workspace picker" onClose={vi.fn()}/>);
    const dialog = screen.getByRole('dialog', { name: 'Workspace picker' });
    expect(dialog.getAttribute('aria-label')).toBe('Workspace picker');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    result.rerender(<Dialog open title="Hidden heading" hideTitleRow onClose={vi.fn()}/>);
    expect(screen.getByRole('dialog', { name: 'Hidden heading' }).getAttribute('aria-label')).toBe('Hidden heading');
    expect(screen.queryByText('Hidden heading')).toBeNull();
  });
});
