import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { RecipeEditor } from './public.tsx';

afterEach(cleanup);
it('closes Recipe confirmation on Cancel and ignores a late delete completion', async () => {
  let finish!: () => void;
  const remove = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
  const close = vi.fn();
  render(<RecipeEditor recipe={{ id: 'recipe', title: 'Ship', body: 'Checklist', revision: 1, created_at: 1, updated_at: 1 }}
    theme="light" onWrite={vi.fn()} onDelete={remove} onClose={close} onCreated={null} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Delete' }));
  expect(screen.getByRole('dialog', { name: 'Delete this recipe?' })).toBeTruthy();
  await user.click(screen.getByRole('button', { name: 'Delete recipe' }));
  expect(remove).toHaveBeenCalledOnce();
  expect(screen.getByRole('button', { name: 'Deleting…' }).getAttribute('aria-busy')).toBe('true');
  expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Cancel' }).disabled).toBe(false);
  await user.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(screen.queryByRole('dialog')).toBeNull();
  await act(async () => { finish(); await Promise.resolve(); });
  expect(close).not.toHaveBeenCalled();
  expect(screen.getByRole('button', { name: 'Delete' })).toBeTruthy();
});
