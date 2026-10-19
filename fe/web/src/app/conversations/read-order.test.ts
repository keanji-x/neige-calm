import { expect, it } from 'vitest';
import { numberReads } from './read-order.ts';

it('retains the newest read number through a retained-window cache update', async () => {
  type Result = { latest: number[]; retained: number[][] };
  let sequence = 0;
  const reads = numberReads<number[], Result>(() => ++sequence, result => result.latest);
  const fetch = reads.queryFn((page: number[]) => Promise.resolve(page));
  const newest = await fetch([61, 62]);
  const initial = reads.structuralSharing(undefined, { latest: newest, retained: [] }) as Result;
  const before = reads.startOf(initial);
  const earlier = await fetch([1, 2]);
  const expanded = reads.structuralSharing(initial, { latest: initial.latest, retained: [earlier] }) as Result;
  expect(reads.latest()).toBe(2);
  expect(reads.startOf(expanded)).toBe(before);
});

it('stamps an equal latest response with its actual later read while an unobserved cache value proves nothing', async () => {
  type Result = { latest: number[]; retained: number[][] };
  let sequence = 0;
  const reads = numberReads<number[], Result>(() => ++sequence, result => result.latest);
  const fetch = reads.queryFn(() => Promise.resolve([61, 62]));
  const initial = reads.structuralSharing(undefined, { latest: await fetch(undefined), retained: [] }) as Result;
  const refreshed = reads.structuralSharing(initial, { latest: await fetch(undefined), retained: [] }) as Result;
  expect(refreshed).toBe(initial);
  expect(reads.startOf(refreshed)).toBe(2);
  const unknown = reads.structuralSharing(refreshed, { latest: [61, 62, 63], retained: [] }) as Result;
  expect(reads.startOf(unknown)).toBe(0);
});
