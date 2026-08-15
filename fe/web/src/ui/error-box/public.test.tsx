// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ErrorBox } from './public.tsx';

afterEach(cleanup);

describe('ErrorBox', () => {
  it('renders its decorative dot and readable message', () => {
    const { container } = render(<ErrorBox message="Could not load transcript" onRetry={vi.fn()} />);
    const dot = container.querySelector('[aria-hidden="true"]');
    expect(dot).toBeTruthy();
    expect(screen.getByText('Could not load transcript')).toBeTruthy();
  });
});
