import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { ReportDocument } from './public.tsx';

afterEach(() => { cleanup(); document.getSelection()?.removeAllRanges(); });

it('links issue references and Markdown in the real Reference task fields', async () => {
  const openFile = vi.fn();
  render(<ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'context', kind: 'prose', payload: { markdown: '[Source issue](https://github.com/example/project/issues/2420)' } },
    { id: 'task', kind: 'task', payload: { key: 'investigate', kind: 'codex', declared_by: 'spec', ready: true,
      goal: '调查 issue #2420，读取 [日志](./evidence.log)。',
      acceptance: 'PR #2484 已发布；验证 fe/core/sample.ts:10-20。' } },
  ] }} fileRoot="/repo" onOpenFileLink={openFile} empty={null} />);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(screen.getByText('investigate', { exact: true }));
  await waitFor(() => { expect(screen.getByRole('button', { name: 'issue #2420' })).toBeTruthy(); });
  expect(screen.getByRole('button', { name: 'PR #2484' })).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: '日志' }));
  expect(openFile).toHaveBeenLastCalledWith({ path: 'evidence.log' });
  await userEvent.click(screen.getByRole('button', { name: 'fe/core/sample.ts:10-20' }));
  expect(openFile).toHaveBeenLastCalledWith({ path: 'fe/core/sample.ts' });
});

it('keeps task link admission, literals, nesting and disclosure intact', async () => {
  const openFile = vi.fn();
  render(<ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'context', kind: 'prose', payload: { markdown: '[Repo](https://github.com/other/repository/issues/1)\n\nBare PR #7 and `fe/core/context.ts:4`.' } },
    { id: 'task', kind: 'task', payload: { key: 'safe', kind: 'codex', declared_by: 'spec', ready: true,
      goal: '[Already issue #6](https://github.com/other/repository/issues/6), ../../outside.ts:1-2 and [bad](javascript:alert(1)).\n\n![external image](https://external.invalid/pixel.png)\n\n```text\nissue #8 fe/core/literal.ts\n```',
      acceptance: 'issue #9；[file](./inside.ts:3)' } },
    { id: 'terminal', kind: 'task', payload: { key: 'command', kind: 'terminal', declared_by: 'spec', ready: true,
      command: 'echo "issue #10 fe/core/command.ts"', gate: { steps: [{ name: 'check', cmd: 'echo "PR #11"' }] } } },
  ] }} fileRoot="/repo" onOpenFileLink={openFile} empty={null} />);
  expect(screen.getByRole('button', { name: 'PR #7' }).querySelector('button')).toBeNull();
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(screen.getByText('safe', { exact: true }));
  expect(screen.getByRole('button', { name: 'Already issue #6' }).querySelector('button')).toBeNull();
  expect(screen.queryByRole('button', { name: '../../outside.ts:1-2' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'bad' })).toBeNull();
  expect(document.querySelector('img[src="https://external.invalid/pixel.png"]')).toBeNull();
  expect(screen.queryByRole('button', { name: 'issue #8' })).toBeNull();
  expect(screen.getByRole('button', { name: 'issue #9' })).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'file' }));
  expect(openFile).toHaveBeenCalledWith({ path: 'inside.ts' });
  const command = screen.getByText('command', { exact: true }).closest('details');
  expect(command?.textContent).toContain('echo "issue #10 fe/core/command.ts"');
  expect(command?.textContent).toContain('echo "PR #11"');
  expect(command?.querySelector('button')).toBeNull();
});

it('does not borrow repository context from another report or from task code', async () => {
  render(<ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'context', kind: 'prose', payload: { markdown: '[One](https://github.com/one/project/issues/1) [Two](https://github.com/two/project/issues/2)' } },
    { id: 'task', kind: 'task', payload: { key: 'ambiguous', kind: 'codex', declared_by: 'spec', ready: true,
      goal: 'issue #2420', acceptance: 'PR #2484' } },
  ] }} empty={null} />);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(screen.getByText('ambiguous', { exact: true }));
  expect(screen.queryByRole('button', { name: 'issue #2420' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'PR #2484' })).toBeNull();
});

it('links qualified paths in structured table cells without interpreting their data as Markdown', async () => {
  const openFile = vi.fn();
  render(<ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'table', kind: 'table', payload: { columns: [{ key: 'value', label: 'Evidence' }], rows: [
      { value: 'fe/core/view/track-page.ts:212-229；../outside.ts:1-2' },
      { value: '**literal** <script>bad()</script>' },
      { value: 3 },
    ] } },
  ] }} fileRoot="/repo" onOpenFileLink={openFile} empty={null} />);
  await userEvent.click(screen.getByRole('button', { name: 'fe/core/view/track-page.ts:212-229' }));
  expect(openFile).toHaveBeenCalledWith({ path: 'fe/core/view/track-page.ts' });
  expect(screen.queryByRole('button', { name: '../outside.ts:1-2' })).toBeNull();
  expect(screen.getByText('**literal** <script>bad()</script>')).toBeTruthy();
  expect(document.querySelector('[data-nc-report] script')).toBeNull();
  expect(screen.getByText('3', { exact: true })).toBeTruthy();
});


it.each([
  ['[Own repository](https://github.com/second/project/issues/1)\n\nissue #2', 'https://github.com/second/project/issues/2'],
  ['issue #2', null],
  ['[One](https://github.com/second/project/issues/1) [Two](https://github.com/third/project/issues/1)\n\nissue #2', null],
])('scopes independent file preview references to their own source: %s', async (markdown, expected) => {
  const report = { summary: '', body: '[Parent](https://github.com/first/project/issues/1)\n\n[Readme](./README.md)', blocks: null };
  render(<ReportDocument report={report} fileRoot="/repo" empty={null} linkPreview={{ trackId: 't', report,
    files: { readFile: (path) => Promise.resolve({ path, text: markdown, size: markdown.length, truncated: false }), rawUrl: (path) => path },
  }} />);
  await userEvent.click(screen.getByRole('button', { name: 'Readme' }));
  await screen.findByText('issue #2', { exact: true });
  if (expected === null) {
    expect(screen.queryByRole('button', { name: 'issue #2' })).toBeNull();
  } else {
    await userEvent.click(screen.getByRole('button', { name: 'issue #2' }));
    await waitFor(() => { expect(within(screen.getByRole('dialog', { name: 'Preview: issue #2' }))
      .getByRole('link', { name: 'Open in new tab ↗' }).getAttribute('href')).toBe(expected); });
  }
});

it('retains the owning report repository in a same-report block preview', async () => {
  const report = { summary: '', body: '', blocks: [
    { id: 'context', kind: 'prose' as const, payload: { markdown: '[Parent](https://github.com/first/project/issues/1) [Details](neige://wave/t#details)' } },
    { id: 'details', kind: 'prose' as const, payload: { markdown: 'issue #2' } },
  ] };
  render(<ReportDocument report={report} empty={null} linkPreview={{ trackId: 't', report }} />);
  await userEvent.click(screen.getByRole('button', { name: 'Details' }));
  const preview = screen.getByRole('dialog', { name: 'Preview: Details' });
  await waitFor(() => { expect(within(preview).getByRole('button', { name: 'issue #2' })).toBeTruthy(); });
});
