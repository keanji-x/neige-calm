import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { EditorView } from '@codemirror/view';
import { EditorState } from '@codemirror/state';
import { language } from '@codemirror/language';
import '../../styles/entry.css';
import { ReadOnlyCode, MarkdownCode, resolveCodeLanguage, CODE_HIGHLIGHT_LIMITS } from './public.tsx';

afterEach(() => {
  cleanup();
  // Native selections must not refer to detached CodeMirror nodes in the next fixture.
  document.getSelection()?.removeAllRanges();
  vi.restoreAllMocks();
  delete document.documentElement.dataset.theme;
});

async function codeView() {
  const element = await screen.findByRole('textbox', { name: 'Code' });
  const view = EditorView.findFromDOM(element);
  if (view === null) throw new Error('CodeMirror view missing');
  return view;
}

it('keeps unknown code as explicitly labelled plain text and copies the original bytes', async () => {
  const write = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
  const text = '<script>danger()</script>\r\n  raw\ttext\n';
  const { container } = render(<MarkdownCode code={text} language="unknown-language" />);
  const view = await codeView();
  expect(view.state.facet(language)).toBeNull();
  expect(screen.getByText('Plain text')).toBeTruthy();
  expect(container.querySelector('script')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Copy code' }));
  expect(write.mock.calls).toEqual([[text]]);
  expect(screen.getByRole('status').textContent).toBe('Copied');
  write.mockRejectedValueOnce(new Error('Clipboard denied'));
  await userEvent.click(screen.getByRole('button', { name: 'Copy code' }));
  expect(screen.getByRole('status').textContent).toBe('Copy failed');
});

it('is keyboard readable and read-only without consuming the search shortcut', async () => {
  render(<ReadOnlyCode text="fn main() {}" source={{ kind: 'filename', value: 'main.rs' }} />);
  const view = await codeView();
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('rust'); });
  expect(view.state.facet(EditorState.readOnly)).toBe(true);
  expect(screen.getByText('1', { exact: true })).toBeTruthy();
  expect(view.contentDOM.getAttribute('contenteditable')).toBe('true');
  expect(view.contentDOM.getAttribute('aria-readonly')).toBe('true');
  await userEvent.click(screen.getByRole('button', { name: 'Copy code' }));
  await userEvent.tab();
  expect(document.activeElement).toBe(view.contentDOM);
  await userEvent.keyboard('{ArrowRight}{ArrowRight}');
  expect(view.state.selection.main.head).toBe(2);
  const slash = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
  view.contentDOM.dispatchEvent(slash);
  expect(slash.defaultPrevented).toBe(false);
  await userEvent.keyboard('x{Backspace}');
  await userEvent.paste('replacement');
  expect(view.state.doc.toString()).toBe('fn main() {}');
  expect(view.dom.querySelector('[name="search"]')).toBeNull();
});

it('retains view, selection, focus and both scroll axes when the host changes theme', async () => {
  await page.viewport(900, 700);
  document.documentElement.dataset.theme = 'dark';
  const text = ('const value = "' + 'x'.repeat(250) + '";\n').repeat(120);
  render(<div style={{ width: 420 }}><ReadOnlyCode text={text} source={{ kind: 'filename', value: 'main.js' }} /></div>);
  const view = await codeView();
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('javascript'); });
  await waitFor(() => {
    expect(view.scrollDOM.scrollWidth).toBeGreaterThan(view.scrollDOM.clientWidth);
    expect(view.scrollDOM.scrollHeight).toBeGreaterThan(view.scrollDOM.clientHeight);
  });
  expect(view.dom.querySelector('[aria-hidden="true"]')).not.toBeNull();
  view.focus();
  view.dispatch({ selection: { anchor: 15 } });
  view.scrollDOM.scrollTop = 100;
  view.scrollDOM.scrollLeft = 80;
  await page.screenshot({ path: 'test-results/code-dark.png' });
  document.documentElement.dataset.theme = 'light';
  await waitFor(() => { expect(view.state.facet(EditorView.darkTheme)).toBe(false); });
  expect(await codeView()).toBe(view);
  expect(view.state.selection.main.head).toBe(15);
  expect(view.hasFocus).toBe(true);
  expect(view.scrollDOM.scrollTop).toBe(100);
  expect(view.scrollDOM.scrollLeft).toBe(80);
  await page.screenshot({ path: 'test-results/code-light.png' });
});

it('bounds large-file parsing, keeps all source, and virtualizes the visible lines', async () => {
  const text = 'line\n'.repeat(CODE_HIGHLIGHT_LIMITS.lines + 1);
  render(<ReadOnlyCode text={text} source={{ kind: 'filename', value: 'main.rs' }} />);
  const view = await codeView();
  expect(screen.getByText('Plain text · large file')).toBeTruthy();
  expect(view.state.doc.toString()).toBe(text);
  expect(view.state.facet(language)).toBeNull();
  await waitFor(() => { expect(view.viewport.to).toBeLessThan(text.length); });
});

it('reports a failed grammar load as plain text while preserving the source', async () => {
  const description = resolveCodeLanguage({ kind: 'language', value: 'rust' });
  if (description === null) throw new Error('Rust metadata missing');
  vi.spyOn(description, 'load').mockRejectedValueOnce(new Error('Chunk load failed'));
  render(<MarkdownCode code="fn main() {}" language="rust" />);
  await screen.findByText('Plain text · highlighting unavailable');
  const view = await codeView();
  expect(view.state.facet(language)).toBeNull();
  expect(view.state.doc.toString()).toBe('fn main() {}');
});

it('ignores a previous grammar load after the source changes', async () => {
  const rust = resolveCodeLanguage({ kind: 'language', value: 'rust' });
  if (rust === null) throw new Error('Rust metadata missing');
  const support = await rust.load();
  let finish: (value: typeof support) => void = () => { throw new Error('Load not started'); };
  vi.spyOn(rust, 'load').mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const { rerender } = render(<MarkdownCode code="fn main() {}" language="rust" />);
  await screen.findByText('Loading Rust…');
  const view = await codeView();
  rerender(<MarkdownCode code="def main(): pass" language="python" />);
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('python'); });
  finish(support);
  await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
  expect(await codeView()).toBe(view);
  expect(view.state.facet(language)?.name).toBe('python');
  expect(view.state.doc.toString()).toBe('def main(): pass');
});

it('preserves the reading position as a fenced source grows', async () => {
  const text = 'const value = 1;\n'.repeat(100);
  const { rerender } = render(<MarkdownCode code={text} language="js" />);
  const view = await codeView();
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('javascript'); });
  view.focus();
  view.dispatch({ selection: { anchor: 20 } });
  view.scrollDOM.scrollTop = 100;
  rerender(<MarkdownCode code={text + 'export default value;'} language="js" />);
  expect(await codeView()).toBe(view);
  expect(view.state.selection.main.head).toBe(20);
  expect(view.scrollDOM.scrollTop).toBe(100);
  expect(view.state.doc.toString()).toBe(text + 'export default value;');
});

it('clamps a reading selection to the normalized document when CRLF source shrinks', async () => {
  const { rerender } = render(<MarkdownCode code={'x'.repeat(100)} language="text" />);
  const view = await codeView();
  view.dispatch({ selection: { anchor: 80 } });
  rerender(<MarkdownCode code={'x\r\n'.repeat(30)} language="text" />);
  expect(view.state.doc.toString()).toBe('x\n'.repeat(30));
  expect(view.state.selection.main.head).toBe(view.state.doc.length);
});

it('retains the focused selection when a fence grammar replaces text nodes', async () => {
  const description = resolveCodeLanguage({ kind: 'language', value: 'rust' });
  if (description === null) throw new Error('Rust metadata missing');
  const support = await description.load();
  let finish: (value: typeof support) => void = () => { throw new Error('Load not started'); };
  vi.spyOn(description, 'load').mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  render(<MarkdownCode code="// unchanged context\nfn main() {}" language="rust" />);
  const view = await codeView();
  view.focus();
  await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
  view.dispatch({ selection: { anchor: 20 } });
  expect(view.state.selection.main.head).toBe(20);
  finish(support);
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('rust'); });
  await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
  expect(await codeView()).toBe(view);
  expect(view.hasFocus).toBe(true);
  expect(view.state.selection.main.head).toBe(20);
});
