import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { ReportDocument } from '../document/public.tsx';
import { trackReportLinkUrl } from '../../../../../core/domain/report.ts';

afterEach(cleanup);

describe('report preview placement through the production renderer', () => {
  it('keeps a rendered report preview stable while entering its nested source', async () => {
    await page.viewport(1600, 900);
    const files = { readFile: () => Promise.reject(new Error('Unexpected file read')), rawUrl: (path: string) => path };
    const report = { summary: '', body: `[Other report](${trackReportLinkUrl('other')})`, blocks: null };
    const nested = { summary: '', body: '## Nested report\n\n[Evidence](neige://source/src_0971fbde)', blocks: null };
    render(<div style={{ position: 'absolute', left: 80, top: 160, width: 560,
      ['--document-start' as string]: '0px', ['--document-measure' as string]: '560px' }}>
      <ReportDocument report={report} empty={null} linkPreview={{ trackId: 't1', report, files,
        renderReference: () => <ReportDocument report={nested} empty={null} linkPreview={{ trackId: 'other', report: nested, files,
          renderSource: () => <p>Captured evidence contents.</p>,
        }} />,
      }} />
    </div>);
    await page.getByRole('button', { name: 'Other report', exact: true }).hover();
    const parent = page.getByRole('dialog', { name: 'Preview: Other report', exact: true });
    await expect.element(page.getByRole('heading', { name: 'Nested report', exact: true })).toBeVisible();
    const before = parent.element().getBoundingClientRect();
    await page.getByRole('button', { name: 'Evidence', exact: true }).hover();
    await expect.element(page.getByText('Captured evidence contents.', { exact: true })).toBeVisible();
    for (let frame = 0; frame < 10; frame++) {
      await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
      const after = parent.element().getBoundingClientRect();
      expect(after.left).toBe(before.left);
      expect(after.top).toBe(before.top);
    }
  });

  it('places growing report previews beside the reading column without covering the link', async () => {
    await page.viewport(1200, 800);
    const report = { summary: '', body: '[Notes](./notes.md)', blocks: null };
    render(<div style={{ position: 'absolute', left: 100, top: 640, width: 480, ['--document-start' as string]: '0px', ['--document-measure' as string]: '480px' }}>
      <ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 't1', report, files: {
        readFile: (path) => Promise.resolve({ path, text: '# Long contents\n\n' + 'Reading content.\n\n'.repeat(60), size: 1000, truncated: false }),
        rawUrl: (path) => path,
      } }} />
    </div>);
    await page.getByRole('button', { name: 'Notes', exact: true }).hover();
    await expect.poll(() => page.getByRole('heading', { name: 'Long contents' }).query()).not.toBeNull();
    const link = page.getByRole('button', { name: 'Notes', exact: true }).element().getBoundingClientRect();
    const card = page.getByRole('dialog').element().getBoundingClientRect();
    expect(card.left).toBeGreaterThanOrEqual(592);
    expect(card.right).toBeLessThanOrEqual(1188);
    expect(card.left >= link.right || card.right <= link.left || card.top >= link.bottom || card.bottom <= link.top).toBe(true);
  });
  it('keeps Markdown paragraph spacing and code formatting inside a file hover card', async () => {
    await page.viewport(1200, 800);
    const report = { summary: '', body: '[Notes](./notes.md)', blocks: null };
    render(<ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 't1', report, files: {
      readFile: path => Promise.resolve({ path, text: '# Notes\n\nFirst paragraph.\n\nSecond **paragraph** with `code`.\n\n```rust\nlet answer = 42;\n```', size: 120, truncated: false }),
      rawUrl: path => path,
    } }} />);
    await page.getByRole('button', { name: 'Notes', exact: true }).hover();
    await expect.element(page.getByText('First paragraph.', { exact: true })).toBeVisible();
    const card = page.getByRole('dialog').element();
    const second = card.querySelector('strong')!.closest('p')!;
    expect(parseFloat(getComputedStyle(second).marginBlockStart)).toBeGreaterThan(0);
    expect(card.querySelector('code')?.textContent).toBe('code');
    expect(card.querySelector('pre code')?.textContent).toBe('let answer = 42;');
    await page.screenshot({ path: 'test-results/file-preview-markdown.png' });
  });

  it.each(['rs', 'ts', 'js', 'py', 'sh'])('uses the read-only file renderer for .%s source links', async extension => {
    await page.viewport(1200, 800);
    const report = { summary: '', body: `[Source](./example.${extension})`, blocks: null };
    render(<ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 't1', report, files: {
      readFile: path => Promise.resolve({ path, text: extension === 'rs' ? 'fn main() { let answer = 42; }' : extension === 'py' ? 'def main(): return 42' : extension === 'sh' ? 'echo "hello"' : 'const answer = 42;', size: 32, truncated: false }),
      rawUrl: path => path,
    } }} />);
    await page.getByRole('button', { name: 'Source', exact: true }).hover();
    await expect.poll(() => page.getByRole('dialog').query()?.querySelector('[role="textbox"]') ?? null, { timeout: 10_000 }).not.toBeNull();
    const editor = page.getByRole('dialog').element().querySelector('[role="textbox"]')!;
    expect(editor.getAttribute('contenteditable')).toBe('false');
    expect(editor.textContent).toContain(extension === 'rs' ? 'fn main()' : extension === 'py' ? 'def main()' : extension === 'sh' ? 'echo' : 'const answer');
    expect(editor.querySelectorAll('span').length).toBeGreaterThan(0);
    await page.screenshot({ path: `test-results/file-preview-${extension}.png` });
  });

});
