import { render, waitFor } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { ReportFileViewer } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

function files(path: string, text: string): WorkspaceFilePort {
  return {
    readFile: () => Promise.resolve({ path, size: text.length, text, truncated: false }),
    rawUrl: (value) => `/raw?path=${encodeURIComponent(value)}`,
  };
}

describe('ReportFileViewer', () => {
  it('renders source as a centered Report document, without card or directory chrome', async () => {
    await browserPage.viewport(1200, 800);
    const onFileOpened = vi.fn();
    render(
      <div
        data-testid="frame"
        style={{
          position: 'relative', inlineSize: 1000, blockSize: 600,
          ['--document-measure' as string]: '568px',
        }}
      >
        <ReportFileViewer
          path="src/main.ts"
          files={files('src/main.ts', 'export const x = 1;')}
          fileRoot="/repo"
          wide
          onClose={vi.fn()}
          onFileOpened={onFileOpened}
        />
      </div>,
    );

    await waitFor(() => { expect(onFileOpened).toHaveBeenCalledWith('src/main.ts'); });
    const frame = document.querySelector<HTMLElement>('[data-testid="frame"]')!;
    const layer = document.querySelector<HTMLElement>('[data-nc-report-file-viewer]')!;
    const source = layer.querySelector<HTMLElement>('[data-nc-report-file-source]')!;
    expect(Math.round(layer.getBoundingClientRect().width))
      .toBe(Math.round(frame.getBoundingClientRect().width));
    expect(source.textContent).toContain('export const x = 1;');
    expect(source.getBoundingClientRect().width).toBeGreaterThan(800);
    const frameRect = frame.getBoundingClientRect();
    const sourceRect = source.getBoundingClientRect();
    expect(Math.abs(
      (sourceRect.left - frameRect.left) - (frameRect.right - sourceRect.right),
    )).toBeLessThanOrEqual(1);
    expect(document.activeElement).toBe(layer);
    expect(layer.querySelector('[data-nc-fs-viewer]')).toBeNull();
    expect(layer.textContent).not.toContain('Diff');
  });

  it('renders Markdown through the same prose pipeline as the Report', async () => {
    render(<div
      data-testid="markdown-frame"
      style={{
        position: 'relative', inlineSize: 1000, blockSize: 600,
        ['--document-measure' as string]: '568px',
      }}
    >
      <ReportFileViewer
        path="README.md"
        files={files('README.md', '# README title\n\nA **rendered** paragraph.')}
        fileRoot="/repo"
        wide
        onClose={vi.fn()}
      />
    </div>);

    await waitFor(() => {
      expect(document.querySelector('[data-nc-report]')).not.toBeNull();
    });
    expect(document.querySelector('[data-nc-report-file-source]')).toBeNull();
    expect(document.querySelector('[data-nc-report]')?.textContent)
      .toContain('README title');
    expect(document.querySelector('[data-nc-report]')?.textContent)
      .toContain('A rendered paragraph.');
    const frame = document.querySelector<HTMLElement>('[data-testid="markdown-frame"]')!;
    const heading = document.querySelector<HTMLElement>('[data-nc-report] h2')!;
    expect(heading.getBoundingClientRect().width).toBeGreaterThan(800);
    const frameRect = frame.getBoundingClientRect();
    const headingRect = heading.getBoundingClientRect();
    expect(Math.abs(
      (headingRect.left - frameRect.left) - (frameRect.right - headingRect.right),
    )).toBeLessThanOrEqual(1);
  });

  it('returns to the Report on Escape', () => {
    const onClose = vi.fn();
    render(<div style={{ position: 'relative', blockSize: 400 }}>
      <ReportFileViewer
        path="README.md"
        files={files('README.md', '# README')}
        fileRoot="/repo"
        wide={false}
        onClose={onClose}
      />
    </div>);
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('keeps the Report document within a compact viewport', async () => {
    await browserPage.viewport(500, 700);
    render(<div style={{ position: 'relative', inlineSize: 460, blockSize: 600 }}>
      <ReportFileViewer
        path="src/main.ts"
        files={files('src/main.ts', 'const compact = true;')}
        fileRoot="/repo"
        wide
        onClose={vi.fn()}
      />
    </div>);

    await waitFor(() => {
      expect(document.querySelector('[data-nc-report-file-source]')).not.toBeNull();
    });
    const layer = document.querySelector<HTMLElement>('[data-nc-report-file-viewer]')!;
    expect(layer.scrollWidth).toBeLessThanOrEqual(layer.clientWidth);
  });
});
