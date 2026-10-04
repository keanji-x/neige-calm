import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../../styles/entry.css';
import source from '../../../../../../test-data/native-view-v1.json?raw';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { trackOverlayPayload, type OverlayWire } from '../../../../../core/domain/track.ts';
import { NativeReportView } from './public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(source) as { valid: { rows: { cells: unknown[] }[] }; valid_slots: { rows: unknown[] } };
const template = nativeViewPayloadSchema.parse({ ...fixture.valid_slots, rows: fixture.valid_slots.rows.slice(0, 1) });
const unit = (cell: unknown) => ({ snapshot: { id: 'unit', observedAt: null, producedAt: null }, cell });
const units = { 'capacity.summary': unit(fixture.valid.rows[0].cells[0]), 'capacity.history': unit(fixture.valid.rows[0].cells[1]) };

function lookup(published: Record<string, unknown>) {
  const overlays: OverlayWire[] = Object.entries(published).map(([kind, payload]) => ({
    id: kind, plugin_id: 'operations', entity_kind: 'track', entity_id: 't', kind, payload, updated_at: 0,
  }));
  return (slot: string) => trackOverlayPayload('t', overlays, slot);
}

function cellBoxes(container: HTMLElement) {
  return [...container.querySelector('section > div')!.children].map(cell => {
    const box = cell.getBoundingClientRect();
    return { left: Math.round(box.left), width: Math.round(box.width) };
  });
}

it.each([1440, 390])('a degraded slot keeps the two-wide-end row layout at %i', async width => {
  await page.viewport(width, 1000);
  const frame = (published: Record<string, unknown>) => render(<main style={{ maxInlineSize: 1000, padding: 12 }}>
    <NativeReportView payload={template} resolveOverlay={lookup(published)} /></main>);
  const healthy = frame(units);
  const layout = cellBoxes(healthy.container);
  expect(layout).toHaveLength(2);
  if (width === 1440) expect(layout[1].width).toBeGreaterThan(layout[0].width * 1.5);
  else expect(layout[1].left).toBe(layout[0].left);
  healthy.unmount();
  for (const history of [undefined, { snapshot: 'malformed' }]) {
    const degraded = frame({ ...units, 'capacity.history': history });
    expect(degraded.container.textContent).toContain('已用空间');
    expect(degraded.container.textContent).toContain(history === undefined ? 'Waiting for' : 'cannot be displayed');
    expect(cellBoxes(degraded.container)).toEqual(layout);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    degraded.unmount();
  }
});
