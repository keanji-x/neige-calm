import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import * as markdown from '../../../../../core/markdown/public.ts';
import { deriveReportOutline, readTrackReport, trackReportLinkUrl, type TrackReport } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import { reportDocumentReferenceRepository } from '../../../../../core/domain/report-references.ts';
import { createReportPreparationCache, prepareProse, prepareReportDocument } from './preparation.ts';
import { PreparedProseBlock } from './content.tsx';
import { ReportDocument } from './public.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });
function card(markdown = '# Stored report\n\nEvidence.'): CardWire {
  return { id: 'report', track_id: 'track', kind: 'track-report', title: null, sort: 0,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [
      { id: 'one', kind: 'prose', rev: 1, payload: { markdown } },
    ] }, deletable: false, created_at: 1, updated_at: 1 };
}

it('reuses only the same immutable report card and keeps router caches independent', () => {
  const first = createReportPreparationCache(); const second = createReportPreparationCache();
  const report = card(); const value = first.read([report]);
  expect(first.read([report])).toBe(value);
  expect(first.read([{ ...report, id: 'unrelated', kind: 'notes', payload: {} }, report])).toBe(value);
  expect(second.read([report])).not.toBe(value);
  const changed = { ...report, payload: card('# New report\n\nNew body.').payload };
  expect(first.read([changed])).not.toBe(value);
  expect(first.read([report])).toBe(value);
  expect(first.read([]).kind).toBe('empty');
  expect(first.read([{ ...report, payload: null }]).kind).toBe('empty');
});

it('shares syntax without merging distinct block anchors or changing core metadata policy', () => {
  const source = '# Shared heading\n\n[Issue](https://github.com/example/project/issues/1).';
  const report: TrackReport = { summary: '', body: '', blocks: [
    { id: 'one', kind: 'prose', payload: { markdown: source } },
    { id: 'two', kind: 'prose', payload: { markdown: source } },
  ] };
  const prepared = prepareReportDocument(report);
  expect(prepared.outline).toEqual(deriveReportOutline(report.blocks));
  expect(prepared.referenceRepository).toEqual(reportDocumentReferenceRepository(report));
  render(<ReportDocument report={report} preparation={prepared} empty={null} />);
  const headings = screen.getAllByRole('heading', { name: 'Shared heading' });
  expect(headings.map(heading => heading.id)).toEqual(prepared.outline.map(item => item.blockId));
  expect(new Set(headings.map(heading => heading.id)).size).toBe(2);
});

it('parses each exact report source and excerpt slice once', () => {
  const parse = vi.spyOn(markdown, 'parse');
  const source = '# Heading\n\nVisible **body**.';
  prepareReportDocument({ summary: '', body: '', blocks: [
    { id: 'one', kind: 'prose', payload: { markdown: source } },
    { id: 'two', kind: 'prose', payload: { markdown: source } },
  ] });
  expect(parse.mock.calls.map(([text]) => text)).toEqual([source, '', '\n\nVisible **body**.']);
});

it('preserves empty and legacy report handling and fail-soft raw rendering', () => {
  const empty = prepareReportDocument(null); expect(empty.kind).toBe('empty');
  const legacy: TrackReport = { summary: 'Summary only.', body: '', blocks: null };
  const prepared = prepareReportDocument(legacy);
  expect(prepared.kind).toBe('legacy');
  render(<ReportDocument report={legacy} preparation={prepared} empty={<p>Empty</p>} />);
  expect(screen.getByText('Summary only.')).toBeTruthy(); cleanup();
  const failed = prepareProse('<script>text only</script>', null, () => ({
    status: 'failed', error: { kind: 'markdown-parse', message: 'fixture failure', diagnostics: [] },
  }));
  const view = render(<PreparedProseBlock prepared={failed} />);
  expect(view.container.querySelector('pre')?.textContent).toBe('<script>text only</script>');
  expect(view.container.querySelector('script')).toBeNull();
});

it('retains current callbacks and file admission when prepared syntax is reused', () => {
  const prepared = prepareProse(`[Track](${trackReportLinkUrl('other')}#target) and [File](/allowed/info.md)`, null);
  const oldTargets: unknown[] = []; const currentTargets: unknown[] = [];
  const view = render(<PreparedProseBlock prepared={prepared} fileRoot="/allowed"
    onOpenLink={target => oldTargets.push(target)} onOpenFileLink={target => oldTargets.push(target)} />);
  expect(screen.getByRole('button', { name: 'File' }).getAttribute('title')).toBe('info.md');
  view.rerender(<PreparedProseBlock prepared={prepared} fileRoot="/denied"
    onOpenLink={target => currentTargets.push(target)} onOpenFileLink={target => currentTargets.push(target)} />);
  expect(screen.queryByRole('button', { name: 'File' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Track' }));
  expect(oldTargets).toEqual([]); expect(currentTargets).toEqual([{ trackId: 'other', blockId: 'target' }]);
  view.rerender(<PreparedProseBlock prepared={prepared} fileRoot="/allowed"
    onOpenLink={target => currentTargets.push(target)} onOpenFileLink={target => currentTargets.push(target)} />);
  fireEvent.click(screen.getByRole('button', { name: 'File' }));
  expect(currentTargets.at(-1)).toEqual({ path: 'info.md' });
});

it('rejects a preparation supplied for another report identity', () => {
  const one = readTrackReport([card()]); const other = readTrackReport([card('# Other')]);
  expect(() => render(<ReportDocument report={other} preparation={prepareReportDocument(one)} empty={null} />))
    .toThrow('Report preparation does not match the displayed report');
});
