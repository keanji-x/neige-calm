import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from '../../../dialog/public.tsx';
import { DirectoryField } from './public.tsx';

const listDirectory = () => Promise.resolve({ path: '/work', parent: '/', entries: [] });
beforeEach(() => { vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; }); vi.stubGlobal('cancelAnimationFrame', vi.fn()); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe('DirectoryField integration', () => {
  it('uses the owning Dialog child-view path without nesting a dialog', async () => {
    render(<Dialog open title="Settings" onClose={vi.fn()}><DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/></Dialog>);
    fireEvent.click(screen.getByRole('button', { name: '/workBrowse…' }));
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeTruthy();
  });

  it('falls back to an inline browser outside Dialog', async () => {
    render(<DirectoryField value="/work" onChange={vi.fn()} listDirectory={listDirectory}/>);
    fireEvent.click(screen.getByRole('button', { name: '/workBrowse…' }));
    expect(await screen.findByRole('combobox')).toBeTruthy();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
