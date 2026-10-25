import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { ReportFileViewer } from './public.tsx';

afterEach(cleanup);

function setup() {
  const close = vi.fn();
  const files = {
    readFile: (path: string) => Promise.resolve({ path, text: '[Evidence](neige://source/src_0971fbde)', size: 48, truncated: false }),
    rawUrl: (path: string) => `/raw/${path}`,
  };
  render(<ReportFileViewer path="notes.md" files={files} fileRoot="/repo" wide onClose={close}
    linkPreview={{ files, trackId: 't1', report: null, renderSource: () =>
      <DropdownMenu button={{ label: 'Evidence actions' }}><DropdownMenuItem label="Evidence item" /></DropdownMenu> }} />);
  return close;
}

it.each([{ isComposing: true }, { keyCode: 229 }])('claims composing file viewer Escape %j', (ime) => {
  const close = setup();
  const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true, ...ime });
  document.querySelector('[data-nc-report-file-viewer]')!.dispatchEvent(event);
  expect(close).not.toHaveBeenCalled();
  expect(event.defaultPrevented).toBe(true);
});

it('dismisses a native preview menu and preview before its file viewer', async () => {
  const close = setup();
  fireEvent.keyDown(await screen.findByRole('button', { name: 'Evidence' }), { key: 'ArrowDown' });
  await page.getByRole('button', { name: 'Evidence actions' }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Evidence item' })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
  await expect.element(page.getByRole('menuitem', { name: 'Evidence item' })).not.toBeInTheDocument();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
  await expect.element(page.getByRole('dialog', { name: 'Preview: Evidence' })).not.toBeInTheDocument();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});

it.each([{ isComposing: true }, { keyCode: 229 }])('keeps file search and its match during composing Enter and Escape %j', async (ime) => {
  const { FileViewer } = await import('../../../systems/fs-viewers/public.tsx');
  const slots = new Map<string, unknown>();
  render(<FileViewer path="/repo" theme="dark" slots={{
    get: <Value,>(key: string, initial: Value | (() => Value)): Value => {
      if (!slots.has(key)) slots.set(key, typeof initial === 'function' ? (initial as () => Value)() : initial);
      return slots.get(key) as Value;
    },
    set: (key, value) => { slots.set(key, value); },
  }} files={{
    readFile: path => Promise.resolve({ path, text: 'alpha alpha alpha', size: 17, truncated: false }),
    rawUrl: path => `/raw/${path}`,
    listDirectory: () => Promise.resolve({ path: '/repo', parent: '/', entries: [{ name: 'notes.txt', is_dir: false }] }),
    gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }),
    gitDiff: path => Promise.resolve({ path, status: 'modified', head_text: '', working_text: '', truncated: false }),
  }} />);
  await page.getByRole('button', { name: 'notes.txt', exact: true }).click();
  await page.getByRole('textbox', { name: 'File code' }).click();
  await userEvent.keyboard('/');
  await page.getByRole('searchbox', { name: 'Search in file' }).fill('alpha');
  await expect.element(page.getByText(/^[1-3]\/3$/)).toBeVisible();
  const before = screen.getByText(/^[1-3]\/3$/).textContent;
  const search = screen.getByRole('searchbox', { name: 'Search in file' });
  fireEvent.keyDown(search, { key: 'Enter', ...ime });
  expect(fireEvent.keyDown(search, { key: 'Escape', ...ime })).toBe(true);
  expect(screen.getByText(/^[1-3]\/3$/).textContent).toBe(before);
  expect(screen.getByRole('searchbox')).toBe(search);
  fireEvent.keyDown(search, { key: 'Escape' });
  expect(screen.queryByRole('searchbox')).toBeNull();
});
