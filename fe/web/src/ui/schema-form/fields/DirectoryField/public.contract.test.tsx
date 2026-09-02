import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DirectoryField } from './public.tsx';

afterEach(cleanup);

describe('DirectoryField public contract', () => {
  /* The name is the button's own contents — the basename — and nothing this
     component adds, because a second call site names this control with a
     `<label htmlFor>` that any `aria-label` here would outrank. What the
     contents cannot carry travels beside them: the full path as the accessible
     *description*, and purpose + path in the native `title` for the pointer. */
  it('exposes a dialog-popup browse button named by its own contents', () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={() => Promise.resolve({ path: '/work', parent: '/', entries: [] })}/>);
    const button = screen.getByRole('button', { name: 'work' });
    expect(button.getAttribute('aria-haspopup')).toBe('dialog');
    expect(button.getAttribute('title')).toBe('Choose a directory: /work');
    // Shown: the segment that identifies the folder, and no `Browse…` beside it.
    expect(button.textContent).toBe('work');
    const described = document.getElementById(button.getAttribute('aria-describedby') ?? '');
    expect(described?.textContent).toBe('/work');
  });
});
