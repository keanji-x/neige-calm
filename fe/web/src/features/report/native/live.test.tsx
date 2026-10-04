// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { readTrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from '../document/public.tsx';
import { ReportTableBlock } from '../table/public.tsx';
import { ReportLiveViewBlock } from './live.tsx';

afterEach(cleanup);
const fixture = JSON.parse(readFileSync(resolve(process.cwd(), '../test-data/native-view-v1.json'), 'utf8')) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);
const source = { source: 'neige://plugin/museum/collection', version: 1 as const };

it('uses the same native composition and controls for inline and live report entry points', async () => {
  const inline = render(<ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'inline', kind: 'view', payload }] }} empty={null} />);
  const presentation = inline.container.textContent;
  const classes = inline.container.querySelector('#inline > div')?.className;
  inline.unmount();
  const live = render(<ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'live', kind: 'view.live', payload: source }] }}
    empty={null} resolveOverlay={() => payload} />);
  expect(live.container.textContent).toBe(presentation);
  expect(live.container.querySelector('#live > div')?.className).toBe(classes);
  await userEvent.click(screen.getByRole('button', { name: '队列' }));
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  await userEvent.click(screen.getByRole('button', { name: '展开 运营概览' }));
  const dialog = within(screen.getByRole('dialog'));
  expect(dialog.getByRole('button', { name: '队列' }).getAttribute('aria-pressed')).toBe('true');
  expect(dialog.getByRole('region', { name: '备份是否按时完成？ 详情' })).toBeTruthy();
  expect(live.container.querySelector('iframe')).toBeNull();
});

it.each(['museum', 'paper-trading', 'longbridge', 'other-publisher'])('renders the same inert contract for publisher %s', publisher => {
  render(<ReportLiveViewBlock payload={{ ...source, source: `neige://plugin/${publisher}/collection` }} resolveOverlay={() => payload} />);
  expect(screen.getByRole('heading', { name: payload.title })).toBeTruthy();
  expect(screen.getByRole('button', { name: '查看详情' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /批准|下单/ })).toBeNull();
});

it('preserves the complete 8000-character live review in text disclosures', async () => {
  const view = structuredClone(payload);
  const records = view.rows[2].cells[0];
  if (records.kind !== 'records') throw new Error('Expected records fixture');
  const body = 'x'.repeat(7983) + '<script></script>';
  records.datasets[0].items[0].disclosures = [{ id: 'long', label: 'Full review', body, note: 'Publisher note', tone: 'neutral' }];
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => view} />);
  await userEvent.click(screen.getByRole('button', { name: '查看详情' }));
  await userEvent.click(screen.getByRole('button', { name: 'Full review' }));
  expect(screen.getByRole('region', { name: 'Full review' }).querySelector('blockquote')?.textContent).toBe(body);
  expect(document.querySelector('script')).toBeNull();
});

it.each([{ ...payload, version: 99 }, { ...payload, rows: [] }, { version: 1, view: 'overview', metrics: [] }])('rejects malformed or old preset compositions instead of guessing', raw => {
  render(<ReportLiveViewBlock payload={source} resolveOverlay={() => raw} />);
  expect(screen.getByRole('status').textContent).toContain('does not match');
  expect(screen.queryByRole('heading', { name: payload.title })).toBeNull();
});

it('retains explicit missing-overlay states', () => {
  const result = render(<ReportLiveViewBlock payload={source} />);
  expect(screen.getByText('This view does not carry live data.')).toBeTruthy();
  result.rerender(<ReportLiveViewBlock payload={source} resolveOverlay={() => undefined} />);
  expect(screen.getByText(`Waiting for ${source.source} — nothing has been pushed here yet.`)).toBeTruthy();
});

it('does not reinterpret a native composition as a legacy table overlay', () => {
  render(<ReportTableBlock payload={{ source: source.source }} resolveLive={() => payload} />);
  expect(screen.getByRole('status').textContent).toContain('cannot be displayed: this build cannot read it as a table');
  expect(screen.queryByRole('heading', { name: payload.title })).toBeNull();
});

it('recognizes an explicit live reference containing only source and version', () => {
  const report = readTrackReport([{
    id: 'report', kind: 'track-report', track_id: 't', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { blocks: [{ id: 'collection', kind: 'view.live', payload: source }] },
  }]);
  expect(report?.blocks?.[0]).toEqual({ id: 'collection', kind: 'view.live', payload: source });
});
