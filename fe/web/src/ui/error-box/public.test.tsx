// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
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
