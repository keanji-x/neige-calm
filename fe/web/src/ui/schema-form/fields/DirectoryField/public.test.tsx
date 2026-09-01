import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from '../../../dialog/public.tsx';
import { useState } from '../../../state/public.ts';
import { DirectoryField } from './public.tsx';

const listDirectory = () => Promise.resolve({ path: '/work', parent: '/', entries: [] });
beforeEach(() => { vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; }); vi.stubGlobal('cancelAnimationFrame', vi.fn()); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe('DirectoryField integration', () => {
  /* The purpose phrase is now half of the accessible name, so a wrong default
     is a wrong name and not a stale placeholder. Red when the default stops
     following `mode`. */
  it('names itself after what it picks when the call site gives no placeholder', () => {
    render(<DirectoryField value="" onChange={vi.fn()} listDirectory={listDirectory} mode="file"/>);
    expect(screen.getByRole('button', { name: 'Choose a file' })).toBeTruthy();
    cleanup();
    render(<DirectoryField value="" onChange={vi.fn()} listDirectory={listDirectory}/>);
    expect(screen.getByRole('button', { name: 'Choose a directory' })).toBeTruthy();
  });

  /* A call site that passes an empty placeholder must not produce a control
     named ": /work" — or, unset, one with no name at all. */
  it('falls back to the default purpose when the placeholder is blank', () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory} placeholder=""/>);
    expect(screen.getByRole('button', { name: 'Choose a directory: /work' })).toBeTruthy();
  });

  it('uses the owning Dialog child-view path without nesting a dialog', async () => {
    render(<Dialog open title="Settings" onClose={vi.fn()}><DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/></Dialog>);
    fireEvent.click(screen.getByRole('button', { name: 'Choose a directory: /work' }));
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeTruthy();
  });

  it('falls back to an inline browser outside Dialog', async () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/>);
    fireEvent.click(screen.getByRole('button', { name: 'Choose a directory: /work' }));
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
    fireEvent.click(screen.getByRole('button', { name: 'Choose a file: /a/b' }));
    fireEvent.click(await screen.findByRole('option', { name: 'c.txt' }));
    fireEvent.click(screen.getByRole('button', { name: 'Choose a file: /a/b/c.txt' }));
    expect(await screen.findByRole('option', { name: 'c.txt' })).toBeTruthy();
    expect(browse).toHaveBeenLastCalledWith('/a/b');
  });
});
