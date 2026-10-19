import { cleanup, render, screen } from '@testing-library/react';
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

it('offers isolation only with a folder, defaults to worktree and omits the isolation key', async () => {
  const { onSubmit } = mount();
  expect(screen.getByRole<HTMLInputElement>('radio', { name: 'Worktree (isolated)' }).checked).toBe(true);
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenCalledWith({ message: 'Plan', cwd: '/repo' });
});

it('marks the direct option, submits none, and clearing the folder resets the choice', async () => {
  const { onSubmit } = mount();
  const direct = screen.getByRole('radio', { name: 'Directly in this folder' });
  expect(screen.getByText('Not isolated · no guarantees')).toBeTruthy();
  await userEvent.click(direct);
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenLastCalledWith({ message: 'Plan', cwd: '/repo', isolation: 'none' });
  await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
  expect(screen.queryByRole('radiogroup', { name: 'Workspace isolation' })).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenLastCalledWith({ message: 'Plan' });
});

it('does not offer isolation for managed folders', () => {
  mount(null);
  expect(screen.queryByRole('radiogroup', { name: 'Workspace isolation' })).toBeNull();
});

it('locks the isolation choice with an unconfirmed request', () => {
  const { props, rerender } = mount();
  rerender(<NewTrackForm {...props} locked />);
  expect(screen.getByRole<HTMLInputElement>('radio', { name: 'Directly in this folder' }).disabled).toBe(true);
});
