// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import { EditableTitle } from './public.tsx';

afterEach(cleanup);

it('keeps the rejected draft in edit mode and reports the rename failure', async () => {
  render(<EditableTitle
    value="Old name"
    editLabel="Rename wave"
    inputLabel="Wave title"
    onCommit={() => Promise.reject(new Error('Rename was rejected.'))}
  />);
  await userEvent.click(screen.getByRole('button', { name: 'Rename wave' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), 'My unsaved name{Enter}');
  expect((await screen.findByRole('alert')).textContent).toContain('Rename was rejected.');
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Wave title' }).value).toBe('My unsaved name');
});

it('submits only once when Enter is followed by blur while the rename is pending', async () => {
  let resolve: () => void = () => undefined;
  const onCommit = vi.fn(() => new Promise<void>((done) => { resolve = done; }));
  render(<><EditableTitle value="Old" editLabel="Rename wave" inputLabel="Wave title" onCommit={onCommit} />
    <button type="button">Elsewhere</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename wave' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), 'New{Enter}');
  await userEvent.click(screen.getByRole('button', { name: 'Elsewhere' }));
  expect(onCommit).toHaveBeenCalledTimes(1);
  resolve();
});

it('lets blur leave edit mode after a rejected rename without retrying it', async () => {
  const onCommit = vi.fn(() => Promise.reject(new Error('No permission')));
  render(<><EditableTitle value="Old" editLabel="Rename wave" inputLabel="Wave title" onCommit={onCommit} />
    <button type="button">Elsewhere</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename wave' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), 'New{Enter}');
  await screen.findByRole('alert');
  await userEvent.click(screen.getByRole('button', { name: 'Elsewhere' }));
  expect(onCommit).toHaveBeenCalledTimes(1);
  expect(screen.getByRole('button', { name: 'Rename wave' })).toBeTruthy();
});
