// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ErrorBox } from './public.tsx';

afterEach(cleanup);

describe('ErrorBox', () => {
  it('renders its decorative dot and readable message', () => {
    const { container } = render(<ErrorBox message="Could not load transcript" onRetry={vi.fn()} />);
    const dot = container.querySelector('span[aria-hidden="true"][class]:not([class=""])');
    expect(dot).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('Could not load transcript');
  });
});


it('keeps retry focused while pending and prevents repeated requests', async () => {
  const onRetry = vi.fn();
  const user = userEvent.setup();
  const { rerender } = render(<ErrorBox message="Unavailable" onRetry={onRetry} />);
  const action = screen.getByRole('button', { name: 'Retry' });
  action.focus();
  rerender(<ErrorBox message="Unavailable" onRetry={onRetry} pending />);
  expect(document.activeElement).toBe(action);
  expect(action.getAttribute('aria-busy')).toBe('true');
  expect(action.getAttribute('aria-disabled')).toBe('true');
  await user.click(action);
  expect(onRetry).not.toHaveBeenCalled();
  rerender(<ErrorBox message="Unavailable" onRetry={onRetry} />);
  await user.click(action);
  expect(onRetry).toHaveBeenCalledTimes(1);
});
