import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from '../../../dialog/public.tsx';
import { useState } from '../../../state/public.ts';
import { DirectoryField } from './public.tsx';

const listDirectory = () => Promise.resolve({ path: '/work', parent: '/', entries: [] });
beforeEach(() => { vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; }); vi.stubGlobal('cancelAnimationFrame', vi.fn()); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe('DirectoryField integration', () => {
  it('uses the owning Dialog child-view path without nesting a dialog', async () => {
    render(<Dialog open title="Settings" onClose={vi.fn()}><DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/></Dialog>);
    fireEvent.click(screen.getByRole('button', { name: '/work' }));
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeTruthy();
  });

  it('falls back to an inline browser outside Dialog', async () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/>);
    fireEvent.click(screen.getByRole('button', { name: '/work' }));
    expect(await screen.findByRole('combobox')).toBeTruthy();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('reopens a selected file from its containing directory', async () => {
    const browse = vi.fn((path?: string) => Promise.resolve({
      path: path ?? '/a/b', parent: '/a', entries: [{ name: 'c.txt', path: '/a/b/c.txt', isDirectory: false }],
    }));
    function Harness() {
      const [value, setValue] = useState('/a/b');
      return <DirectoryField value={value} onChange={setValue} listDirectory={browse} mode="file"/>;
    }
    render(<Harness/>);
    fireEvent.click(screen.getByRole('button', { name: '/a/b' }));
    fireEvent.click(await screen.findByRole('option', { name: 'c.txt' }));
    fireEvent.click(screen.getByRole('button', { name: '/a/b/c.txt' }));
    expect(await screen.findByRole('option', { name: 'c.txt' })).toBeTruthy();
    expect(browse).toHaveBeenLastCalledWith('/a/b');
  });
});
