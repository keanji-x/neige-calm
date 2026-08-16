import { render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ErrorBox } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('ErrorBox browser styles', () => {
  it('vertically centers the inline-block decorative dot', () => {
    const { container } = render(<ErrorBox message="Could not load transcript" onRetry={vi.fn()} />);
    const dot = container.querySelector<HTMLElement>('span[aria-hidden="true"]');
    expect(dot).not.toBeNull();
    expect(getComputedStyle(dot!).verticalAlign).toBe('middle');
  });
});
