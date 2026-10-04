// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { nativeViewPayloadSchema, type NativeViewPayload } from '../../../../../core/domain/report-view.ts';
import { trackOverlayPayload, type OverlayWire } from '../../../../../core/domain/track.ts';
import { NativeReportView } from './public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(readFileSync(resolve(process.cwd(), '../test-data/native-view-v1.json'), 'utf8')) as {
  valid: { rows: { cells: Record<string, unknown>[] }[] }; valid_slots: unknown;
};
const template = nativeViewPayloadSchema.parse(fixture.valid_slots);
const metrics = fixture.valid.rows[0].cells[0];
const series = fixture.valid.rows[0].cells[1];
const unit = (cell: unknown, observedAt: number | null = 1790035200000) => ({ snapshot: { id: 'unit-r1', observedAt, producedAt: null }, cell });

/** The production exact lookup over overlay wires the way the Track detail delivers them. */
function lookup(units: Record<string, unknown>) {
  const overlays: OverlayWire[] = Object.entries(units).map(([kind, payload]) => ({
    id: kind, plugin_id: 'operations', entity_kind: 'track', entity_id: 't', kind, payload, updated_at: 0,
  }));
  return (source: string) => trackOverlayPayload('t', overlays, source);
}

function view(cells: unknown[], layout: string): NativeViewPayload {
  return nativeViewPayloadSchema.parse({ version: 1, title: '容量', description: '', snapshot: null,
    rows: [{ id: 'row', title: '概览', layout, cells }] });
}
const slot = (id: string, kind: string, expects: string) => ({ kind: 'live', id, source: `neige://plugin/operations/${kind}`, expects });

it('degrades only the failing slot', () => {
  const payload = view([slot('ok', 'ok', 'metrics'), slot('pending', 'pending', 'time-series'), slot('malformed', 'malformed', 'table')], 'three');
  const { container } = render(<NativeReportView payload={payload} resolveOverlay={lookup({ ok: unit(metrics), malformed: { cell: 'not a unit' } })} />);
  expect(screen.getByText('已用空间').parentElement?.textContent).toContain('120 GB');
  expect(screen.getByText('Waiting for neige://plugin/operations/pending — nothing has been pushed here yet.')).toBeTruthy();
  expect(screen.getByRole('status').textContent).toBe('neige://plugin/operations/malformed cannot be displayed: it is not a data unit this build can read.');
  const cells = container.querySelector('section > div')!.children;
  expect(cells).toHaveLength(3);
});

it('shows a wrong-kind unit as unavailable', () => {
  const payload = view([slot('ok', 'ok', 'metrics'), slot('wrong', 'wrong', 'time-series')], 'two');
  render(<NativeReportView payload={payload} resolveOverlay={lookup({ ok: unit(metrics), wrong: unit(metrics) })} />);
  expect(screen.getAllByText('已用空间')).toHaveLength(1);
  expect(screen.getByRole('status').textContent).toContain('it holds a metrics cell where the template expects time-series');
});

it('keys each unit by its slot, so equal unit-local ids stay distinct', async () => {
  const payload = view([slot('first', 'first', 'time-series'), slot('second', 'second', 'time-series')], 'two');
  render(<NativeReportView payload={payload} resolveOverlay={lookup({ first: unit(series), second: unit(series) })} />);
  const [first, second] = screen.getAllByRole('button', { name: '合计' });
  await userEvent.click(first);
  expect(first.getAttribute('aria-pressed')).toBe('true');
  expect(second.getAttribute('aria-pressed')).toBe('false');
});

it('lists each resolved unit in the snapshot disclosure, reading unknown times as unknown', async () => {
  render(<NativeReportView payload={template} resolveOverlay={lookup({
    'capacity.summary': unit(metrics), 'capacity.history': unit(series, null),
    'capacity.detail': unit(fixture.valid.rows[1].cells[1]),
  })} />);
  await userEvent.click(screen.getByRole('button', { name: '快照信息' }));
  expect(screen.getByText('容量 · 资料截止 2026-09-22T00:00:00.000Z · 生成 未知')).toBeTruthy();
  expect(screen.getByText('历史用量 · 资料截止 未知 · 生成 未知')).toBeTruthy();
  expect(screen.getByText('明细 · 资料截止 2026-09-22T00:00:00.000Z · 生成 未知')).toBeTruthy();
});

it('says a surface without live data carries none, and omits empty provenance', () => {
  render(<NativeReportView payload={template} />);
  expect(screen.getAllByText('This view does not carry live data.')).toHaveLength(3);
  expect(screen.queryByRole('button', { name: '快照信息' })).toBeNull();
});

it('gives empty titles fallback accessible names and omits their headings', async () => {
  const { container } = render(<NativeReportView payload={template} />);
  expect(container.querySelector('h2')).toBeNull();
  expect(container.querySelectorAll('h3')).toHaveLength(1);
  expect(screen.getAllByRole('region').map(region => region.getAttribute('aria-label'))).toEqual(['明细']);
  expect(container.querySelector('section:not([aria-label])')).toBeTruthy();
  expect(container.querySelector('p[class*="description"]')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '放大查看' }));
  const dialog = screen.getByRole('dialog', { name: '视图' });
  expect(within(dialog).queryByRole('heading', { level: 2 })).toBeNull();
});
