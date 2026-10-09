import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { ReportDocument } from './public.tsx';

afterEach(() => { cleanup(); document.getSelection()?.removeAllRanges(); });

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
