import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../styles/entry.css';
import { FileViewer, type ViewerSlots } from './public.tsx';

afterEach(cleanup);

it('recovers a missing selected file in the real code viewer', async () => {
  await page.viewport(960, 650);
  const values = new Map<string, unknown>();
  const slots: ViewerSlots = {
    get<T,>(key: string, initial: T | (() => T)) {
      if (!values.has(key)) values.set(key, typeof initial === 'function' ? (initial as () => T)() : initial);
      return values.get(key) as T;
    },
    set: (key, value) => { values.set(key, value); },
  };
  const readFile = vi.fn().mockRejectedValueOnce(new Error('Path not found: /repo/notes.txt'))
    .mockResolvedValueOnce({ path: '/repo/notes.txt', size: 8, text: 'RESTORED_FILE', truncated: false });
  render(<div style={{ height: 500 }}><FileViewer path="/repo" theme="dark" slots={slots} files={{
    listDirectory: () => Promise.resolve({ path: '/repo', parent: '/', entries: [{ name: 'notes.txt', is_dir: false }] }),
    readFile,
    gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }),
    gitDiff: vi.fn(), rawUrl: (path) => path,
  }} /></div>);
  await userEvent.click(await screen.findByRole('button', { name: /notes\.txt/ }));
  expect(await screen.findByText('File or folder not found.')).toBeTruthy();
  expect(screen.getByText('Path not found: /repo/notes.txt').checkVisibility()).toBe(false);
  await page.screenshot({ path: 'test-results/file-read-failed.png' });
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  expect(await screen.findByText('RESTORED_FILE', {}, { timeout: 10_000 })).toBeTruthy();
  expect(readFile.mock.calls).toEqual([['/repo/notes.txt'], ['/repo/notes.txt']]);
  expect(screen.queryByRole('alert')).toBeNull();
  await page.screenshot({ path: 'test-results/file-read-restored.png' });
});

it('opens a hidden file and navigates a hidden directory in the real viewer', async () => {
  await page.viewport(960, 650);
  const values = new Map<string, unknown>();
  const slots: ViewerSlots = {
    get<T,>(key: string, initial: T | (() => T)) {
      if (!values.has(key)) values.set(key, typeof initial === 'function' ? (initial as () => T)() : initial);
      return values.get(key) as T;
    },
    set: (key, value) => { values.set(key, value); },
  };
  const listDirectory = vi.fn((path: string) => Promise.resolve({
    path, parent: path === '/repo' ? '/' : '/repo',
    entries: path === '/repo'
      ? [{ name: '.config', is_dir: true }, { name: '.env', is_dir: false }]
      : [{ name: 'settings.txt', is_dir: false }],
  }));
  const readFile = vi.fn((path: string) => Promise.resolve({
    path, size: 11, text: 'HIDDEN_FILE', truncated: false,
  }));
  render(<div style={{ height: 500 }}><FileViewer path="/repo" theme="dark" slots={slots} files={{
    listDirectory, readFile,
    gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }),
    gitDiff: vi.fn(), rawUrl: (path) => path,
  }} /></div>);
  expect((await screen.findByRole('button', { name: /\.config/ })).checkVisibility()).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: /\.env/ }));
  expect(await screen.findByText('HIDDEN_FILE', {}, { timeout: 10_000 })).toBeTruthy();
  expect(readFile.mock.calls).toEqual([['/repo/.env']]);
  await page.screenshot({ path: 'test-results/hidden-file-visible.png' });
  await userEvent.click(screen.getByRole('button', { name: /\.config/ }));
  expect(await screen.findByRole('button', { name: /settings\.txt/ })).toBeTruthy();
  expect(listDirectory.mock.calls).toEqual([['/repo'], ['/repo/.config']]);
});
