import { z } from 'zod';
import { dataUnitShapeSchema, nativeViewShapeSchema } from './report-view.generated.js';
import type { NativeView, Component, DataUnit, LiveSlot, Snapshot } from './report-view.types.generated.js';
import { inlineTableBlockPayloadSchema, rejectReservedObjectKeys } from './report-table.js';

export type NativeViewPayload = NativeView;
export type NativeComponent = Component;
export type NativeLiveSlot = LiveSlot;
export type NativeSnapshot = Snapshot;
export type DataUnitPayload = DataUnit;
const MAX_READ_BYTES = 4 * 1024 * 1024;

/** UTF-8 size of the compact JSON encoding, counted only until it passes the budget; `null` when the value is not JSON. */
function encodedBytes(value: unknown): number | null {
  let encoded: string;
  try { encoded = JSON.stringify(value); } catch { return null; }
  if (encoded === undefined) return null;
  let bytes = 0;
  for (const char of encoded) {
    const point = char.codePointAt(0)!;
    bytes += point < 128 ? 1 : point < 2048 ? 2 : point < 65536 ? 3 : 4;
    if (bytes > MAX_READ_BYTES) return bytes;
  }
  return bytes;
}

/** Independent decoded-resource budget; exact canonical write admission belongs to the kernel. */
function readBudget(value: unknown, ctx: z.RefinementCtx) {
  const bytes = encodedBytes(value);
  if (bytes === null) ctx.addIssue({ code: 'custom', message: 'Expected JSON presentation' });
  else if (bytes > MAX_READ_BYTES) ctx.addIssue({ code: 'custom', message: 'Presentation exceeds 4 MiB read budget' });
}

type Issue = (message: string) => void;
function unique(issue: Issue, items: readonly { id: string }[], message: string) {
  if (new Set(items.map(item => item.id)).size !== items.length) issue(message);
}

/** Relations inside one component, shared by inline cells and data units. */
function componentRelations(component: Component, issue: Issue) {
  switch (component.kind) {
    case 'metrics':
      unique(issue, component.items, 'Duplicate metric');
      if (component.items.filter(item => item.emphasis === 'primary').length > 1) issue('At most one primary metric');
      break;
    case 'time-series':
      unique(issue, component.datasets, 'Duplicate dataset');
      for (const data of component.datasets) {
        unique(issue, data.series, 'Duplicate series');
        for (let index = 0; index < data.points.length; index++) {
          const point = data.points[index];
          if (point.values.length !== data.series.length) issue('Point width must match series');
          if (index > 0 && point.date <= data.points[index - 1].date) issue('Dates must increase');
          if (data.style === 'stacked' && point.values.some(value => value === null || value < 0)) issue('Stacked data must be complete and nonnegative');
        }
      }
      break;
    case 'distribution': unique(issue, component.slices, 'Duplicate slice'); break;
    case 'table':
      if (!inlineTableBlockPayloadSchema.safeParse(component.table).success) issue('Invalid inline table');
      break;
    case 'records':
      unique(issue, component.datasets, 'Duplicate dataset');
      for (const data of component.datasets) {
        unique(issue, data.items, 'Duplicate record');
        for (const item of data.items) unique(issue, item.disclosures, 'Duplicate disclosure');
      }
      break;
    case 'meter':
      if (component.limit !== null && component.limit <= 0) issue('Meter limit must be positive');
      break;
    case 'bars': break;
  }
}

/** Relations between fields cannot be expressed by the structural JSON Schema.
 * The kernel independently enforces these on admission; no business decisions live here. */
function relations(view: NativeView, ctx: z.RefinementCtx) {
  const issue: Issue = message => ctx.addIssue({ code: 'custom', message });
  unique(issue, view.rows, 'Duplicate row');
  unique(issue, view.rows.flatMap(row => row.cells), 'Duplicate cell');
  const inline = view.rows.some(row => row.cells.some(cell => cell.kind !== 'live'));
  if (inline && view.snapshot === null) issue('A view with an inline cell needs a snapshot');
  if (!inline && view.snapshot !== null) issue('A view of live slots only has a null snapshot');
  for (const row of view.rows) {
    const width = row.layout === 'one' ? 1 : row.layout === 'three' ? 3 : 2;
    if (row.cells.length !== width) issue('Layout must match cell count');
    for (const cell of row.cells) if (cell.kind !== 'live') componentRelations(cell, issue);
  }
}

export const nativeViewPayloadSchema: z.ZodType<NativeViewPayload> = z.unknown()
  .superRefine(readBudget).superRefine(rejectReservedObjectKeys)
  .pipe(nativeViewShapeSchema as z.ZodType<NativeViewPayload>).superRefine(relations);

/** The unit envelope decoder: its own snapshot and exactly one cell, never a live slot. */
const dataUnitEnvelopeSchema: z.ZodType<DataUnit> = z.unknown().superRefine(rejectReservedObjectKeys)
  .pipe(dataUnitShapeSchema as z.ZodType<DataUnit>);

export type LiveSlotResolution =
  | Readonly<{ state: 'ok'; unit: DataUnitPayload }>
  | Readonly<{ state: 'pending' }>
  | Readonly<{ state: 'unavailable'; reason: string }>;

/**
 * Resolves one live slot on its own, through the injected exact overlay lookup. Nothing
 * published is `pending`. A unit over the read budget, not shaped as a data unit, holding
 * another kind than the slot expects, or whose cell breaks its relations is `unavailable`.
 */
export function resolveLiveSlot(slot: LiveSlot, lookup: (source: string) => unknown): LiveSlotResolution {
  const raw = lookup(slot.source);
  if (raw === undefined) return { state: 'pending' };
  const bytes = encodedBytes(raw);
  if (bytes === null || bytes > MAX_READ_BYTES) return { state: 'unavailable', reason: 'it exceeds the 4 MiB read budget' };
  const envelope = dataUnitEnvelopeSchema.safeParse(raw);
  if (!envelope.success) return { state: 'unavailable', reason: 'it is not a data unit this build can read' };
  const unit = envelope.data;
  if (unit.cell.kind !== slot.expects) {
    return { state: 'unavailable', reason: `it holds a ${unit.cell.kind} cell where the template expects ${slot.expects}` };
  }
  const issues: string[] = [];
  componentRelations(unit.cell, message => issues.push(message));
  if (issues.length > 0) return { state: 'unavailable', reason: `its ${unit.cell.kind} cell is invalid: ${issues[0]}` };
  return { state: 'ok', unit };
}
