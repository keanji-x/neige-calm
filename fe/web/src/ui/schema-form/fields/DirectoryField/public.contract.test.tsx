import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DirectoryField } from './public.tsx';

afterEach(cleanup);

describe('DirectoryField public contract', () => {
  /* The name is the *whole* path even though the chip shows only the basename:
     "app" is not an answer to "which folder", and the name is all a screen
     reader gets. The title carries the same string for the pointer. */
  it('exposes a dialog-popup browse button with the current path as its name', () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={() => Promise.resolve({ path: '/work', parent: '/', entries: [] })}/>);
    const button = screen.getByRole('button', { name: '/work' });
    expect(button.getAttribute('aria-haspopup')).toBe('dialog');
    expect(button.getAttribute('title')).toBe('/work');
    // Shown: the segment that identifies the folder, and no `Browse…` beside it.
    expect(button.textContent).toBe('work');
  });
});
