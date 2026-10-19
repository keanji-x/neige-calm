import { cleanup, render, screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

function mount(cwd: string | null = '/repo') {
  const onSubmit = vi.fn();
  const props = { submitting: false, error: null, templates: [], templatesLoaded: true,
    loadTemplate: vi.fn(), initialTemplateId: null, initialCwd: cwd,
    onManageRecipes: vi.fn(), listDirectory: vi.fn(), onSubmit };
  return { onSubmit, props, ...render(<NewTrackForm {...props} />) };
}

it('keeps a worktree menu inside the composer and omits the default isolation key', async () => {
  const { onSubmit } = mount();
  const trigger = screen.getByRole('button', { name: 'Workspace isolation: Worktree' });
  expect(trigger.closest('[data-nc-composer-preferences]')).toBeTruthy();
  expect(trigger.closest('[data-nc-new-track-message]')).toBeTruthy();
  await userEvent.click(trigger);
  expect(within(screen.getByRole('menuitem', { name: /Worktree/ })).getByText('Selected')).toBeTruthy();
  await userEvent.keyboard('{Escape}');
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenCalledWith({ message: 'Plan', cwd: '/repo' });
});

it('submits none with a warning, and clearing the folder restores private None without a warning', async () => {
  const { onSubmit } = mount();
  await userEvent.click(screen.getByRole('button', { name: 'Workspace isolation: Worktree' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'None' }));
  expect(screen.getByRole('button', { name: 'Not isolated · no guarantees' })).toBeTruthy();
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenLastCalledWith({ message: 'Plan', cwd: '/repo', isolation: 'none' });
  await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
  expect(screen.getByRole('button', { name: 'Workspace isolation: None' })).toBeTruthy();
  expect(screen.queryByText('Not isolated · no guarantees')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenLastCalledWith({ message: 'Plan' });
});

it('shows private None selected and announces Worktree as disabled with a reason', async () => {
  const { onSubmit } = mount(null);
  await userEvent.click(screen.getByRole('button', { name: 'Workspace isolation: None' }));
  const worktree = screen.getByRole('menuitem', { name: /Worktree/ });
  expect(worktree.getAttribute('aria-disabled')).toBe('true');
  expect(screen.getByText('Private Neige folder: no worktree. Choose a Git folder to use one.')).toBeTruthy();
  expect(within(screen.getByRole('menuitem', { name: /None/ })).getByText('Selected')).toBeTruthy();
  await userEvent.click(worktree);
  expect(screen.getByRole('button', { name: 'Workspace isolation: None' })).toBeTruthy();
  expect(screen.queryByText('Not isolated · no guarantees')).toBeNull();
  await userEvent.keyboard('{Escape}');
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenCalledWith({ message: 'Plan' });
});

it('restores a folder draft choice and locks the menu with an unconfirmed request', () => {
  const { props, rerender } = mount();
  rerender(<NewTrackForm key="draft" {...props} initialDraft={{ message: 'Plan', selected: { kind: 'none' },
    templateValues: {}, cwd: '/repo', isolation: 'none' }} />);
  expect(screen.getByRole('button', { name: 'Workspace isolation: None' })).toBeTruthy();
  rerender(<NewTrackForm key="draft" {...props} locked />);
  expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Workspace isolation: None' }).disabled).toBe(true);
});

it('selects isolation using only the keyboard', async () => {
  mount();
  screen.getByRole('button', { name: 'Workspace isolation: Worktree' }).focus();
  await userEvent.keyboard('{ArrowDown}');
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: /Worktree/ })));
  await userEvent.keyboard('{ArrowDown}{Enter}');
  expect(screen.getByRole('button', { name: 'Workspace isolation: None' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Not isolated · no guarantees' })).toBeTruthy();
});
