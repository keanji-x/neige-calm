import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { EditorState } from '@codemirror/state';
import { getSearchQuery } from '@codemirror/search';
import { resolveCodeLanguage } from '../../ui/code/public.tsx';
import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { syntaxTree } from '@codemirror/language';
import { DiffPane } from './code-pane.tsx';
import { CodePane, type PaneSearchAdapter } from './code-pane.tsx';

afterEach(() => {
  cleanup();
  document.getSelection()?.removeAllRanges();
  vi.restoreAllMocks();
});

it.each([
  ['main.rs', 'rust', 'fn main() {}'],
  ['main.py', 'python', 'def main(): pass'],
  ['main.js', 'javascript', 'const main = 1;'],
  ['main.ts', 'typescript', 'const main: number = 1;'],
  ['main.jsx', 'javascript', 'const main = <div />;'],
  ['main.tsx', 'typescript', 'const main = <div />;'],
  ['main.sh', 'shell', 'echo hello'],
  ['Dockerfile', '', 'FROM alpine'],
])('loads the registered grammar for %s in the real file pane', async (path, name, text) => {
  const { container } = render(<CodePane path={path} text={text} theme="dark" />);
  await waitFor(() => {
    const editor = container.querySelector('[role="textbox"]');
    expect(editor).not.toBeNull();
    const view = editor === null ? null : EditorView.findFromDOM(editor as HTMLElement);
    expect(view?.state.facet(language)?.name.toLowerCase()).toBe(name);
  }, { timeout: 10000 });
});

it.each(['main.jsx', 'main.tsx'])('parses JSX syntax for %s rather than plain JS/TS', async path => {
  const { container } = render(<CodePane path={path} text="const main = <div />;" theme="light" />);
  await waitFor(() => {
    const editor = container.querySelector<HTMLElement>('[role="textbox"]');
    const view = editor === null ? null : EditorView.findFromDOM(editor);
    let found = false;
    if (view !== null) syntaxTree(view.state).iterate({ enter: node => { if (node.name === 'JSXElement') found = true; } });
    expect(found).toBe(true);
  });
});

it('loads the same filename grammar for both real diff sides', async () => {
  const { container } = render(<DiffPane path="main.rs" headText="fn old() {}" workingText="fn main() {}" theme="light" />);
  await waitFor(() => {
    const editors = container.querySelectorAll<HTMLElement>('[role="textbox"]');
    expect(editors.length).toBe(2);
    for (const editor of editors) {
      const view = EditorView.findFromDOM(editor);
      expect(view?.state.facet(language)?.name).toBe('rust');
      expect(view?.state.facet(EditorState.readOnly)).toBe(true);
      expect(editor.getAttribute('aria-readonly')).toBe('true');
    }
  });
});

it('retains the full file pane search adapter and slash shortcut', async () => {
  const ready: { adapter: PaneSearchAdapter | null } = { adapter: null };
  const count = vi.fn();
  const open = vi.fn();
  const { container } = render(<CodePane path="main.rs" text="let value = value;" theme="dark"
    onSearchAdapterReady={value => { ready.adapter = value; }} onSearchCount={count} onSlashOpen={open} />);
  await waitFor(() => { expect(ready.adapter).not.toBeNull(); });
  // The callback populates the adapter after the pane creates its real view.
  const searchAdapter = ready.adapter;
  if (searchAdapter === null) throw new Error('Search adapter missing');
  searchAdapter.setQuery('value');
  expect(count).toHaveBeenLastCalledWith(1, 2);
  searchAdapter.next();
  expect(count).toHaveBeenLastCalledWith(2, 2);
  searchAdapter.prev();
  expect(count).toHaveBeenLastCalledWith(1, 2);
  const content = container.querySelector<HTMLElement>('[role="textbox"]');
  if (content === null) throw new Error('Full pane missing');
  content.focus();
  expect(document.activeElement).toBe(content);
  const slash = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
  content.dispatchEvent(slash);
  expect(slash.defaultPrevented).toBe(true);
  expect(open).toHaveBeenCalledOnce();
});

it('retains both diff views and expanded context when the grammar arrives', async () => {
  const description = resolveCodeLanguage({ kind: 'filename', value: 'main.rs' });
  if (description === null) throw new Error('Rust metadata missing');
  const support = await description.load();
  let finish: (value: typeof support) => void = () => { throw new Error('Load not started'); };
  vi.spyOn(description, 'load').mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const context = '// unchanged context\n'.repeat(40);
  const { container } = render(<DiffPane path="main.rs" headText={context + 'fn old() {}'} workingText={context + 'fn new() {}'} theme="light" />);
  const elements = container.querySelectorAll<HTMLElement>('[role="textbox"]');
  const left = EditorView.findFromDOM(elements[0]);
  const right = EditorView.findFromDOM(elements[1]);
  if (left === null || right === null) throw new Error('Diff views missing');
  screen.getAllByText(/unchanged lines/)[0].click();
  expect(screen.queryAllByText(/unchanged lines/)).toHaveLength(0);
  left.focus();
  expect(left.hasFocus).toBe(true);
  left.dispatch({ selection: { anchor: 20 } });
  expect(left.state.selection.main.head).toBe(20);
  finish(support);
  await waitFor(() => {
    const current = container.querySelectorAll<HTMLElement>('[role="textbox"]');
    const currentLeft = EditorView.findFromDOM(current[0]);
    const currentRight = EditorView.findFromDOM(current[1]);
    expect(currentLeft?.state.facet(language)?.name).toBe('rust');
    expect(currentLeft).toBe(left);
    expect(currentRight).toBe(right);
  });
  expect(left.state.selection.main.head).toBe(20);
  expect(left.hasFocus).toBe(true);
  expect(screen.queryAllByText(/unchanged lines/)).toHaveLength(0);
});

it('retains full-file selection and search state when a grammar replaces text nodes', async () => {
  const description = resolveCodeLanguage({ kind: 'filename', value: 'main.rs' });
  if (description === null) throw new Error('Rust metadata missing');
  const support = await description.load();
  let finish: (value: typeof support) => void = () => { throw new Error('Load not started'); };
  vi.spyOn(description, 'load').mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const ready: { adapter: PaneSearchAdapter | null } = { adapter: null };
  const { container } = render(<CodePane path="main.rs" text="// unchanged context\nfn main() {}" theme="dark"
    onSearchAdapterReady={adapter => { ready.adapter = adapter; }} />);
  const content = container.querySelector<HTMLElement>('[role="textbox"]');
  if (content === null) throw new Error('Full file pane missing');
  const view = EditorView.findFromDOM(content);
  if (view === null) throw new Error('Full file view missing');
  await waitFor(() => { expect(ready.adapter).not.toBeNull(); });
  ready.adapter?.setQuery('main');
  view.focus();
  await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
  view.dispatch({ selection: { anchor: 20 } });
  expect(view.state.selection.main.head).toBe(20);
  finish(support);
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('rust'); });
  await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
  expect(EditorView.findFromDOM(content)).toBe(view);
  expect(view.state.facet(EditorState.readOnly)).toBe(true);
  expect(view.hasFocus).toBe(true);
  expect(view.state.selection.main.head).toBe(20);
  expect(getSearchQuery(view.state).search).toBe('main');
});
