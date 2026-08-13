// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

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
