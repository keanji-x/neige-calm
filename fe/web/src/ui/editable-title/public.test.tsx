// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import { EditableTitle } from './public.tsx';

afterEach(cleanup);

it('keeps the rejected draft in edit mode and reports the rename failure', async () => {
  render(<EditableTitle
    value="Old name"
    editLabel="Rename track"
    inputLabel="Track title"
    onCommit={() => Promise.reject(new Error('Rename was rejected.'))}
  />);
  await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'My unsaved name{Enter}');
  expect((await screen.findByRole('alert')).textContent).toContain('Rename was rejected.');
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Track title' }).value).toBe('My unsaved name');
});

it('submits only once when Enter is followed by blur while the rename is pending', async () => {
  let resolve: () => void = () => undefined;
  const onCommit = vi.fn(() => new Promise<void>((done) => { resolve = done; }));
  render(<><EditableTitle value="Old" editLabel="Rename track" inputLabel="Track title" onCommit={onCommit} />
    <button type="button">Elsewhere</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'New{Enter}');
  await userEvent.click(screen.getByRole('button', { name: 'Elsewhere' }));
  expect(onCommit).toHaveBeenCalledTimes(1);
  resolve();
});

it('does not pull focus back after Tab leaves an Enter commit that is still pending', async () => {
  let resolve: () => void = () => undefined;
  const onCommit = () => new Promise<void>((done) => { resolve = done; });
  render(<><EditableTitle value="Old" editLabel="Rename" inputLabel="Title" onCommit={onCommit} />
    <button type="button">Next</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Title' }), 'New{Enter}');
  await userEvent.tab();
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Next' }));
  resolve();
  await screen.findByRole('button', { name: 'Rename' });
  await new Promise((done) => requestAnimationFrame(done));
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Next' }));
});

it('lets blur leave edit mode after a rejected rename without retrying it', async () => {
  const onCommit = vi.fn(() => Promise.reject(new Error('No permission')));
  render(<><EditableTitle value="Old" editLabel="Rename track" inputLabel="Track title" onCommit={onCommit} />
    <button type="button">Elsewhere</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'New{Enter}');
  await screen.findByRole('alert');
  await userEvent.click(screen.getByRole('button', { name: 'Elsewhere' }));
  expect(onCommit).toHaveBeenCalledTimes(1);
  expect(screen.getByRole('button', { name: 'Rename track' })).toBeTruthy();
});

it.each(['Enter', 'Escape'])('returns focus to the title after %s', async (key) => {
  render(<EditableTitle value="Old" editLabel="Rename" inputLabel="Title" onCommit={() => undefined} />);
  await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
  fireEvent.keyDown(screen.getByRole('textbox', { name: 'Title' }), { key });
  const title = await screen.findByRole('button', { name: 'Rename' });
  await waitFor(() => expect(document.activeElement).toBe(title));
});

it('lets Tab move away after blur commits a changed title', async () => {
  render(<><EditableTitle value="Old" editLabel="Rename" inputLabel="Title" onCommit={() => undefined} />
    <button type="button">Next</button></>);
  await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
  await userEvent.clear(screen.getByRole('textbox', { name: 'Title' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Title' }), 'New');
  await userEvent.tab();
  await screen.findByRole('button', { name: 'Rename' });
  await new Promise((resolve) => requestAnimationFrame(resolve));
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Next' }));
});

it('suppresses the synthesized click after Enter accepts an unchanged title', async () => {
  render(<EditableTitle value="Old" editLabel="Rename" inputLabel="Title" onCommit={() => undefined} />);
  await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
  fireEvent.keyDown(screen.getByRole('textbox', { name: 'Title' }), { key: 'Enter' });
  const title = await screen.findByRole('button', { name: 'Rename' });
  await waitFor(() => expect(document.activeElement).toBe(title));
  fireEvent.click(title);
  expect(screen.queryByRole('textbox', { name: 'Title' })).toBeNull();
});
