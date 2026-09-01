import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ComponentProps } from 'react';
import { DirectoryBrowser, type DirectoryListing, type ListDirectory } from './public.tsx';

const root: DirectoryListing = { path: '/work', parent: '/', entries: [
  { name: 'src', path: '/work/src', isDirectory: true },
  { name: 'notes.txt', path: '/work/notes.txt', isDirectory: false },
] };
const child: DirectoryListing = { path: '/work/src', parent: '/work', entries: [] };
const listing: ListDirectory = vi.fn((path?: string) => Promise.resolve(path === '/work/src' ? child : root));

beforeEach(() => { vi.clearAllMocks(); vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; }); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

async function ready(props: Partial<ComponentProps<typeof DirectoryBrowser>> = {}) {
  const onCancel = vi.fn(); const onSelect = vi.fn();
  render(<DirectoryBrowser listDirectory={listing} initialPath="/work" onCancel={onCancel} onSelect={onSelect} {...props}/>);
  await screen.findByRole('option', { name: 'src' });
  return { input: screen.getByRole('combobox'), onCancel, onSelect };
}

describe('DirectoryBrowser behavior', () => {
  it('descends into the highlighted directory with slash', async () => {
    const { input } = await ready(); fireEvent.keyDown(input, { key: '/' });
    await waitFor(() => expect(listing).toHaveBeenLastCalledWith('/work/src'));
  });
  it('Escape cancels the entire browser', async () => {
    const { input, onCancel } = await ready(); fireEvent.keyDown(input, { key: 'Escape' }); expect(onCancel).toHaveBeenCalledOnce();
  });
  it('Enter confirms the clean current path when no item is highlighted', async () => {
    const { input, onSelect } = await ready(); fireEvent.change(input, { target: { value: '/work/s' } }); fireEvent.change(input, { target: { value: '/work/' } }); fireEvent.keyDown(input, { key: 'Enter' }); expect(onSelect).toHaveBeenCalledWith('/work');
  });
  it('rejects a non-absolute Enter path', async () => {
    const { input } = await ready(); fireEvent.change(input, { target: { value: 'relative' } }); fireEvent.keyDown(input, { key: 'Enter' }); expect(screen.getByRole('alert').textContent).toBe('Enter an absolute path');
  });
  /* Named by its text and not by `getByRole('status')` alone: every astryx
     `Button` renders an always-present, empty `role="status"` live region of
     its own (`Button.tsx`), so this surface's three buttons make the bare role
     query ambiguous. The assertion is unchanged in what it locks — the row that
     says Loading is a live region — and both halves still fail on their own if
     the row loses its text or its role. */
  it('shows loading while a listing request is pending', () => {
    const pending = new Promise<DirectoryListing>(() => undefined);
    render(<DirectoryBrowser listDirectory={() => pending} initialPath={null} onCancel={vi.fn()} onSelect={vi.fn()}/>);
    const loading = screen.getByText('Loading…');
    expect(loading.getAttribute('role')).toBe('status');
  });
  it('resets active descendant when filtering changes', async () => {
    const { input } = await ready(); expect(input.getAttribute('aria-activedescendant')).toBe(screen.getByRole('option', { name: 'src' }).id);
    fireEvent.change(input, { target: { value: '/work/no' } });
    expect(input.hasAttribute('aria-activedescendant')).toBe(false);
  });
  it('omits active descendant when a directory listing has no interactive option', async () => {
    const filesOnly: DirectoryListing = { path: '/work', parent: '/', entries: [{ name: 'notes.txt', path: '/work/notes.txt', isDirectory: false }] };
    render(<DirectoryBrowser listDirectory={() => Promise.resolve(filesOnly)} initialPath="/work" onCancel={vi.fn()} onSelect={vi.fn()}/>);
    await screen.findByRole('option', { name: 'notes.txt' });
    expect(screen.getByRole('combobox').hasAttribute('aria-activedescendant')).toBe(false);
  });
  it('uses distinct owned listbox and option ids for multiple instances', async () => {
    render(<><DirectoryBrowser listDirectory={listing} initialPath="/work" onCancel={vi.fn()} onSelect={vi.fn()}/>
      <DirectoryBrowser listDirectory={listing} initialPath="/work" onCancel={vi.fn()} onSelect={vi.fn()}/></>);
    const inputs = await screen.findAllByRole('combobox');
    const lists = screen.getAllByRole('listbox');
    const options = screen.getAllByRole('option', { name: 'src' });
    expect(lists[0].id).not.toBe(lists[1].id);
    expect(options[0].id).not.toBe(options[1].id);
    expect(inputs[0].getAttribute('aria-controls')).toBe(lists[0].id);
    expect(inputs[1].getAttribute('aria-controls')).toBe(lists[1].id);
    expect(inputs[0].getAttribute('aria-activedescendant')).toBe(options[0].id);
    expect(inputs[1].getAttribute('aria-activedescendant')).toBe(options[1].id);
  });
  it('walks up to the listing parent from the pointer', async () => {
    await ready();
    fireEvent.click(screen.getByRole('button', { name: 'Parent directory' }));
    await waitFor(() => expect(listing).toHaveBeenLastCalledWith('/'));
  });
  it('offers no way up out of a listing that has no parent', async () => {
    const filesystemRoot: DirectoryListing = { path: '/', parent: null, entries: [{ name: 'work', path: '/work', isDirectory: true }] };
    render(<DirectoryBrowser listDirectory={() => Promise.resolve(filesystemRoot)} initialPath="/" onCancel={vi.fn()} onSelect={vi.fn()}/>);
    await screen.findByRole('option', { name: 'work' });
    expect(screen.getByRole('button', { name: 'Parent directory' }).hasAttribute('disabled')).toBe(true);
  });
  /* Two different facts, and the difference decides what the reader does next:
     back up a character, or navigate somewhere else entirely. */
  it('tells an empty directory apart from a filtered-out one', async () => {
    const { input } = await ready();
    fireEvent.change(input, { target: { value: '/work/zzz' } });
    expect(screen.getByText('No matches')).toBeTruthy();
    fireEvent.change(input, { target: { value: '/work/src/' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(await screen.findByText('Empty directory')).toBeTruthy();
  });
  it('returns focus through two animation frames after loading', async () => {
    const callbacks: FrameRequestCallback[] = [];
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callbacks.push(callback); return callbacks.length; });
    render(<DirectoryBrowser listDirectory={listing} initialPath="/work" onCancel={vi.fn()} onSelect={vi.fn()}/>);
    const input = screen.getByRole('combobox'); document.body.focus();
    await waitFor(() => expect(callbacks).toHaveLength(1)); callbacks.shift()!(0);
    expect(callbacks).toHaveLength(1); callbacks.shift()!(0);
    expect(document.activeElement).toBe(input);
  });
});
