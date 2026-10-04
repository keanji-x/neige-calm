import { describe, expect, it } from 'vitest';
import { resolveLiveSlot, type NativeLiveSlot } from './report-view.js';

const slot: NativeLiveSlot = { kind: 'live', id: 'detail', source: 'neige://plugin/operations/capacity.detail', expects: 'table' };
const table = (rows: Record<string, string>[]) => ({ snapshot: { id: 'unit', observedAt: null, producedAt: null },
  cell: { kind: 'table', id: 'detail', title: '', table: { columns: Object.keys(rows[0]).map(key => ({ key, label: key })), rows } } });

describe('live slot resolution', () => {
  it('asks the injected lookup for exactly the slot source and reports nothing published as pending', () => {
    const asked: string[] = [];
    expect(resolveLiveSlot(slot, source => { asked.push(source); return undefined; })).toEqual({ state: 'pending' });
    expect(asked).toEqual([slot.source]);
  });
  it('accepts a unit of the expected kind up to the 4 MiB read budget and refuses one past it', () => {
    const within = Array.from({ length: 500 }, () => ({ name: '雪'.repeat(2048) }));
    expect(resolveLiveSlot(slot, () => table(within)).state).toBe('ok');
    const oversized = within.map(row => ({ ...row, note: row.name }));
    expect(resolveLiveSlot(slot, () => table(oversized))).toEqual({ state: 'unavailable', reason: 'it exceeds the 4 MiB read budget' });
  });
  it('refuses a live slot posing as a unit cell and a cell that breaks its relations', () => {
    expect(resolveLiveSlot(slot, () => ({ snapshot: { id: 'unit', observedAt: null, producedAt: null }, cell: slot })).state).toBe('unavailable');
    const undeclared = table([{ name: 'a' }]);
    undeclared.cell.table.rows = [{ name: 'a', extra: 'b' }];
    expect(resolveLiveSlot(slot, () => undeclared)).toEqual({ state: 'unavailable', reason: 'its table cell is invalid: Invalid inline table' });
  });
});
