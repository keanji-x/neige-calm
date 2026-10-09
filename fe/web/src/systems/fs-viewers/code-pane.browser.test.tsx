import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { CodePane } from './code-pane.tsx';
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
    const editor = container.querySelector('.cm-editor');
    expect(editor).not.toBeNull();
    const view = editor === null ? null : EditorView.findFromDOM(editor as HTMLElement);
    expect(view?.state.facet(language)?.name.toLowerCase()).toBe(name);
  });
});

it('uses the fence language through the real report renderer', async () => {
  const report = { summary: '', body: '```rust\nfn main() {}\n```', blocks: null };
  const { container } = render(<ReportDocument report={report} empty={null} />);
  await waitFor(() => {
    const editor = container.querySelector('.cm-editor');
    expect(editor).not.toBeNull();
    const view = editor === null ? null : EditorView.findFromDOM(editor as HTMLElement);
    expect(view?.state.facet(language)?.name).toBe('rust');
  });
});
