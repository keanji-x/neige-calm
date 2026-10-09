import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { EditorState } from '@codemirror/state';
import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
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
});

it('highlights a source hover through the file resource owner and keeps its card stable while reading', async () => {
  await page.viewport(1200, 800);
  const text = ('fn main() { let value = "' + 'x'.repeat(200) + '"; }\n').repeat(140);
  const report = { summary: '', body: '[Source](./main.rs)', blocks: null };
  render(<div style={{ position: 'absolute', left: 100, top: 220, width: 480,
    ['--document-start' as string]: '0px', ['--document-measure' as string]: '480px' }}>
    <ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 't1', report, files: {
      readFile: path => Promise.resolve({ path, text, size: text.length, truncated: false }), rawUrl: path => path,
    } }} />
  </div>);
  await page.getByRole('button', { name: 'Source', exact: true }).hover();
  const element = await screen.findByRole('textbox', { name: 'Code' });
  const view = EditorView.findFromDOM(element);
  if (view === null) throw new Error('Source view missing');
  await waitFor(() => { expect(view.state.facet(language)?.name).toBe('rust'); });
  const card = screen.getByRole('dialog');
  const before = card.getBoundingClientRect();
  const trigger = page.getByRole('button', { name: 'Source', exact: true }).element().getBoundingClientRect();
  expect(before.left >= trigger.right || before.right <= trigger.left || before.top >= trigger.bottom || before.bottom <= trigger.top).toBe(true);
  view.focus();
  view.scrollDOM.scrollTop = 120;
  view.scrollDOM.scrollLeft = 60;
  await page.getByRole('textbox', { name: 'Code' }).hover();
  for (let frame = 0; frame < 10; frame++) {
    await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
    const after = card.getBoundingClientRect();
    expect(after.left).toBe(before.left);
    expect(after.top).toBe(before.top);
  }
  expect(view.scrollDOM.scrollTop).toBe(120);
  expect(view.scrollDOM.scrollLeft).toBe(60);
  expect(view.state.facet(EditorState.readOnly)).toBe(true);
  expect(element.getAttribute('aria-readonly')).toBe('true');
  await page.screenshot({ path: 'test-results/code-source-hover.png' });
  element.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await waitFor(() => { expect(screen.queryByRole('dialog')).toBeNull(); });
});
