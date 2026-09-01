import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DirectoryField } from './public.tsx';

afterEach(cleanup);

describe('DirectoryField public contract', () => {
  /* The name is the control's purpose *and* the whole path, even though the
     chip shows only the basename: "app" answers neither "which folder" nor
     "what is this control", and the name is all a screen reader gets. The
     title carries the same string for the pointer. */
  it('exposes a dialog-popup browse button with the current path as its name', () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={() => Promise.resolve({ path: '/work', parent: '/', entries: [] })}/>);
    const button = screen.getByRole('button', { name: 'Choose a directory: /work' });
    expect(button.getAttribute('aria-haspopup')).toBe('dialog');
    expect(button.getAttribute('title')).toBe('Choose a directory: /work');
    // Shown: the segment that identifies the folder, and no `Browse…` beside it.
    expect(button.textContent).toBe('work');
  });
});
