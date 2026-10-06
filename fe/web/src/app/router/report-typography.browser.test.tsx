import '../../styles/entry.css';
import { cleanup, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { renderPage } from '../../features/track/page/test-fixtures.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportFileViewer } from '../../features/report/file-viewer/public.tsx';
import shell from '../shell/shell.module.css';

const markdown = '# Findings\n\n报告正文保持稳定的行长。窗口变窄时自动重排，打开文件时继续使用相同的阅读宽度。';
const files: WorkspaceFilePort = {
  readFile: () => Promise.resolve({ path: 'report.md', size: markdown.length, text: markdown, truncated: false }),
  rawUrl: () => '/raw',
};

afterEach(() => {
  cleanup();
  document.documentElement.style.removeProperty('font-size');
});

function renderReport(file = false, wide = false) {
  const view = renderPage({
    report: <ReportDocument report={{ summary: '', body: markdown, blocks: null }} empty={<></>} />,
    board: file ? <ReportFileViewer path="report.md" files={files} fileRoot="/repo" wide={wide} onClose={vi.fn()} /> : undefined,
  });
  // Use the shell's production container and panel sizing, including its cqi scope.
  view.container.className = shell.main;
  return view;
}

function prose(file = false) {
  return file
    ? document.querySelector<HTMLElement>('[data-nc-report-file-viewer] [data-nc-report] p')!
    : document.querySelector<HTMLElement>('[data-nc-report] p')!;
}

it.each([320, 768, 1000, 1440, 1920])('keeps Report and Markdown reading widths equal at %ipx', async width => {
  await page.viewport(width, 900);
  const view = renderReport();
  const expected = prose().getBoundingClientRect();
  expect(expected.width).toBeLessThanOrEqual(640);
  expect(expected.width).toBeGreaterThan(200);
  view.unmount();
  for (const wide of [false, true]) {
    const fileView = renderReport(true, wide);
    await waitFor(() => { expect(prose(true)).not.toBeNull(); });
    const actual = prose(true).getBoundingClientRect();
    expect(actual.width).toBeCloseTo(expected.width, 0);
    expect(actual.left).toBeCloseTo(expected.left, 0);
    const fileName = document.querySelector<HTMLElement>('[data-nc-report-file-viewer] [title="report.md"]')!;
    expect(fileName.getBoundingClientRect().left).toBeCloseTo(actual.left, 0);
    const layer = document.querySelector<HTMLElement>('[data-nc-report-file-viewer]')!;
    expect(layer.scrollWidth).toBeLessThanOrEqual(layer.clientWidth);
    fileView.unmount();
  }
});

it('preserves a bounded reading column on a tablet', async () => {
  await page.viewport(900, 900);
  renderReport();
  expect(prose().getBoundingClientRect().width).toBeCloseTo(640, 0);
});

it('scales the reading ceiling with the root font size without overflowing the container', async () => {
  await page.viewport(1920, 900);
  document.documentElement.style.fontSize = '20px';
  renderReport();
  const paragraph = prose();
  expect(paragraph.getBoundingClientRect().width).toBeCloseTo(800, 0);
  expect(Number.parseFloat(getComputedStyle(paragraph).fontSize)).toBe(22.5);
  const heading = document.querySelector<HTMLElement>('[data-nc-report] h2')!;
  expect(Number.parseFloat(getComputedStyle(heading).fontSize)).toBe(27.5);
  expect(paragraph.getBoundingClientRect().right).toBeLessThanOrEqual(paragraph.closest('[data-nc-report]')!.getBoundingClientRect().right);
});
