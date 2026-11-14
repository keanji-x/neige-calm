import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

it.each(['/repo', null])('uses implicit isolation for folder %s', async (cwd) => {
  const onSubmit = vi.fn();
  render(<NewTrackForm submitting={false} error={null}
    initialCwd={cwd}
    listDirectory={vi.fn()} onSubmit={onSubmit} />);
  expect(screen.queryByRole('button', { name: /Workspace isolation/ })).toBeNull();
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Plan');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenLastCalledWith(cwd === null ? { message: 'Plan' }
    : { message: 'Plan', cwd, isolation: 'none' });
  if (cwd !== null) {
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    expect(onSubmit).toHaveBeenLastCalledWith({ message: 'Plan' });
  }
});

it('labels the creation preferences visibly without changing their selection semantics', () => {
  render(<NewTrackForm submitting={false} error={null}
    initialCwd="/repo"
    listDirectory={vi.fn()} onSubmit={vi.fn()} />);
  expect(screen.queryByRole('button', { name: /^Template:/ })).toBeNull();
  expect(screen.getByRole('button', { name: 'Repo: repo (/repo)' }).textContent).toContain('Repo: repo');
});
