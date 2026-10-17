import '../../styles/entry.css';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { EditorView } from '@codemirror/view';
import { FileViewer, type ViewerSlots } from './public.tsx';

afterEach(cleanup);

async function editorPaint() {
  await act(async () => { await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))); });
}

it.each([false, true])('keeps file search visible and clickable while the editor scrolls (truncated: %s)', async truncated => {
  await page.viewport(1000, 750);
  const values = new Map<string, unknown>();
  const slots: ViewerSlots = {
    get<T,>(key: string, initial: T | (() => T)) {
      if (!values.has(key)) values.set(key, typeof initial === 'function' ? (initial as () => T)() : initial);
      return values.get(key) as T;
    },
    set: (key, value) => { values.set(key, value); },
  };
  const text = Array.from({ length: 300 }, (_, index) => `line ${index} ${[8, 140, 298].includes(index) ? 'needle' : 'ordinary'} ${'.'.repeat(100)}`).join('\n');
  // FileViewer's real card is a shrinking flex column. Keep that production
  // frame; a plain fixed-height parent does not exercise its layout contract.
  render(<div className="fv-card" style={{ height: truncated ? 360 : 480, width: truncated ? 580 : 660 }}>
    <FileViewer path="/repo" theme="light" slots={slots} files={{
      listDirectory: () => Promise.resolve({ path: '/repo', parent: '/', entries: [{ name: 'large.txt', is_dir: false }, { name: 'other.txt', is_dir: false }] }),
      readFile: path => Promise.resolve({ path, size: text.length, text: path.endsWith('large.txt') ? text : 'Different file', truncated }),
      gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }), gitDiff: vi.fn(), rawUrl: path => path,
    }} />
  </div>);
  await page.getByRole('button', { name: /large\.txt/ }).click();
  const content = await screen.findByRole('textbox', { name: 'File code' });
  const view = EditorView.findFromDOM(content);
  if (view === null) throw new Error('Actual file editor not mounted');
  const assertUsable = () => {
    const wrap = screen.getByRole('search').parentElement;
    if (wrap === null) throw new Error('File search frame missing');
    const clip = wrap.getBoundingClientRect();
    expect(wrap.scrollTop).toBe(0);
    for (const control of [screen.getByRole('searchbox', { name: 'Search in file' }),
      screen.getByRole('button', { name: 'Next match' }), screen.getByRole('button', { name: 'Previous match' }), screen.getByRole('button', { name: 'Close search' })]) {
      const rect = control.getBoundingClientRect();
      expect(rect.top).toBeGreaterThanOrEqual(Math.max(0, clip.top));
      expect(rect.bottom).toBeLessThanOrEqual(Math.min(innerHeight, clip.bottom));
      expect(rect.left).toBeGreaterThanOrEqual(Math.max(0, clip.left));
      expect(rect.right).toBeLessThanOrEqual(Math.min(innerWidth, clip.right));
      const hit = document.elementFromPoint((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
      expect(hit !== null && control.contains(hit)).toBe(true);
    }
  };
  content.focus();
  await userEvent.keyboard('/');
  await page.getByRole('searchbox', { name: 'Search in file' }).fill('needle');
  await waitFor(() => expect(screen.getByText('1/3')).toBeDefined());
  await editorPaint();
  assertUsable();
  await page.getByRole('button', { name: 'Next match', exact: true }).click();
  await waitFor(() => expect(screen.getByText('2/3')).toBeDefined());
  await editorPaint();
  assertUsable();
  expect(view.scrollDOM.scrollTop).toBeGreaterThan(0);
  await page.getByRole('button', { name: 'Next match', exact: true }).click();
  await waitFor(() => expect(screen.getByText('3/3')).toBeDefined());
  await editorPaint();
  assertUsable();
  expect(view.state.doc.sliceString(view.state.selection.main.from, view.state.selection.main.to)).toBe('needle');
  await page.getByRole('button', { name: 'Next match', exact: true }).click();
  await waitFor(() => expect(screen.getByText('1/3')).toBeDefined());
  await editorPaint();
  assertUsable();
  await page.getByRole('button', { name: 'Previous match', exact: true }).click();
  await waitFor(() => expect(screen.getByText('3/3')).toBeDefined());
  await editorPaint();
  assertUsable();
  await page.getByRole('button', { name: 'Close search', exact: true }).click();
  expect(screen.queryByRole('search')).toBeNull();
  await page.getByRole('button', { name: /other\.txt/ }).click();
  await screen.findByText('Different file');
  expect(screen.queryByRole('search')).toBeNull();
});
