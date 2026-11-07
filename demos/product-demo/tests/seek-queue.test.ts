import test from "node:test";
import assert from "node:assert/strict";
import { createSeekQueue } from "../live/seek-queue.ts";
const gate = () => {
  let release!: () => void;
  const wait = new Promise<void>((r) => {
    release = r;
  });
  return { wait, release };
};
test("all seek producers serialize and skip obsolete pending targets", async () => {
  const first = gate(),
    entered = gate();
  const applied: number[] = [],
    committed: number[] = [];
  let active = 0,
    maximum = 0;
  const q = createSeekQueue(
    async (t) => {
      active++;
      maximum = Math.max(maximum, active);
      applied.push(t);
      if (t === 49) {
        entered.release();
        await first.wait;
      }
      active--;
    },
    (t) => committed.push(t),
  );
  const old = q.seek(49);
  await entered.wait;
  const middle = q.seek(8),
    reset = q.seek(0);
  first.release();
  await Promise.all([old, middle, reset]);
  assert.deepEqual(applied, [49, 0]);
  assert.deepEqual(committed, [0]);
  assert.equal(maximum, 1);
});
test("a stale in-flight seek cannot publish a completed playhead", async () => {
  const first = gate(),
    entered = gate(),
    last = gate();
  const committed: number[] = [];
  const q = createSeekQueue(
    async (t) => {
      if (t === 49) {
        entered.release();
        await first.wait;
      } else await last.wait;
    },
    (t) => committed.push(t),
  );
  const old = q.seek(49);
  await entered.wait;
  const reset = q.seek(0);
  first.release();
  await old;
  assert.deepEqual(committed, []);
  last.release();
  await reset;
  assert.deepEqual(committed, [0]);
});
test("a failed native operation does not poison later reset requests", async () => {
  const committed: number[] = [];
  const q = createSeekQueue(
    async (t) => {
      if (t === 49) throw Error("native failure");
    },
    (t) => committed.push(t),
  );
  await assert.rejects(q.seek(49), /native failure/);
  await q.seek(0);
  await q.settled();
  assert.deepEqual(committed, [0]);
});
