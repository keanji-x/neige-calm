import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { syntaxTree } from '@codemirror/language';
import { DiffPane } from './code-pane.tsx';
import { CodePane, type PaneSearchAdapter } from './code-pane.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';

afterEach(cleanup);

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
  });
});

it('uses the fence language through the real report renderer', async () => {
  const report = { summary: '', body: '```rust\nfn main() {}\n```', blocks: null };
  const { container } = render(<ReportDocument report={report} empty={null} />);
  await waitFor(() => {
    const editor = container.querySelector('[role="textbox"]');
    expect(editor).not.toBeNull();
    const view = editor === null ? null : EditorView.findFromDOM(editor as HTMLElement);
    expect(view?.state.facet(language)?.name).toBe('rust');
  });
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
      expect(editor.getAttribute('contenteditable')).toBe('false');
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
  const slash = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
  content.dispatchEvent(slash);
  expect(slash.defaultPrevented).toBe(true);
  expect(open).toHaveBeenCalledOnce();
});
