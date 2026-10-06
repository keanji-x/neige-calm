import '../../styles/entry.css';
import { cleanup, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { renderPage } from '../../features/track/page/test-fixtures.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportFileViewer } from '../../features/report/file-viewer/public.tsx';
import shell from '../shell/shell.module.css';
import nativeSource from '../../../../../test-data/native-view-v1.json?raw';
import { nativeViewPayloadSchema } from '../../../../core/domain/report-view.ts';

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

// An inline View is part of the reading column; its inspection dialog owns expansion.
it.each([390, 900, 1440, 1920])('aligns a mixed prose/View report on both edges at %ipx', async width => {
  await page.viewport(width, 1000);
  const fixture = JSON.parse(nativeSource) as { valid: unknown };
  const payload = nativeViewPayloadSchema.parse(fixture.valid);
  const view = renderPage({ report: <ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'before-view', kind: 'prose', payload: { markdown: 'Before the native view.' } },
    { id: 'mixed-view', kind: 'view', payload },
    { id: 'after-view', kind: 'prose', payload: { markdown: 'After the native view.' } },
  ] }} backlinkCounts={new Map([['mixed-view', 3]])} empty={<></>} /> });
  view.container.className = shell.main;
  const before = document.getElementById('before-view')!.getBoundingClientRect();
  const native = document.getElementById('mixed-view')!.getBoundingClientRect();
  const after = document.getElementById('after-view')!.getBoundingClientRect();
  expect(native.left).toBeCloseTo(before.left, 0);
  expect(native.right).toBeCloseTo(before.right, 0);
  expect(after.left).toBeCloseTo(before.left, 0);
  expect(after.right).toBeCloseTo(before.right, 0);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  if (width >= 1440) {
    const note = document.querySelector<HTMLElement>('[title="3 reports cite this block"]')!.getBoundingClientRect();
    expect(note.left).toBeGreaterThanOrEqual(native.right);
    expect(note.top).toBeLessThan(native.top + 40);
  }
});
