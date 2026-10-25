import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ChatComposer } from './public.tsx';

afterEach(cleanup);

it.each([{ isComposing: true }, { keyCode: 229 }])('keeps a composer edit during composing Enter and Escape %j', (ime) => {
  const onSend = vi.fn(); const onCancel = vi.fn();
  render(<ChatComposer onSend={onSend} editing={{ preview: 'Original', onCancel }} />);
  const field = screen.getByRole('textbox');
  fireEvent.input(field, { target: { textContent: 'Draft' } });
  fireEvent.keyDown(field, { key: 'Enter', ...ime });
  expect(fireEvent.keyDown(field, { key: 'Escape', ...ime })).toBe(true);
  expect(onSend).not.toHaveBeenCalled();
  expect(onCancel).not.toHaveBeenCalled();
  fireEvent.keyDown(field, { key: 'Escape' });
  expect(onCancel).toHaveBeenCalledOnce();
});
