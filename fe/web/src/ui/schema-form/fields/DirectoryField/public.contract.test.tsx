import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DirectoryField } from './public.tsx';

afterEach(cleanup);

describe('DirectoryField public contract', () => {
  it('exposes a dialog-popup browse button with the current path as its name', () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={() => Promise.resolve({ path: '/work', parent: '/', entries: [] })}/>);
    const button = screen.getByRole('button', { name: '/workBrowse…' });
    expect(button.getAttribute('aria-haspopup')).toBe('dialog');
    expect(button.getAttribute('title')).toBe('/work');
  });
});
