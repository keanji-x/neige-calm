import { createRef, StrictMode } from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';
import { ChatComposer, type ComposerFocusHandle } from './public.tsx';
import '../../../styles/entry.css';

afterEach(cleanup);

it('reports only the message field or its live parking intent through StrictMode replay', async () => {
  const handle = createRef<ComposerFocusHandle>();
  const scene = (disabled = false, request = 0) => <StrictMode>
    <button type="button">Outside choice</button>
    <ChatComposer onSend={() => {}} disabled={disabled} focusRequest={request} focusHandle={handle}
      footerActions={<button type="button">Composer option</button>} />
  </StrictMode>;
  const view = render(scene());
  const field = document.querySelector<HTMLElement>('[contenteditable]')!;
  expect(handle.current?.ownsFocus()).toBe(false);
  field.focus();
  expect(handle.current?.ownsFocus()).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Composer option' }));
  expect(handle.current?.ownsFocus()).toBe(false);
  field.focus();
  view.rerender(scene(true, 1));
  await expect.poll(() => handle.current?.ownsFocus()).toBe(true);
  expect(document.activeElement).toBe(screen.getByRole('group', { name: 'Message composer' }));
  const outside = screen.getByRole('button', { name: 'Outside choice' });
  await userEvent.click(outside);
  expect(handle.current?.ownsFocus()).toBe(false);
  view.rerender(scene(false, 1));
  expect(document.activeElement).toBe(outside);
  expect(handle.current?.ownsFocus()).toBe(false);
  view.unmount();
  expect(handle.current).toBeNull();
});
