import test from "node:test";
import assert from "node:assert/strict";
import { relayAt, flight, contentTimeAt } from "../live/relay-model.ts";
test("one task changes hands in the requested order before landing in Report", () => {
  assert.equal(relayAt(22.5).owner, "codex");
  assert.equal(relayAt(24).owner, "claude");
  assert.equal(relayAt(26.2).owner, "codex");
  assert.equal(relayAt(28).owner, "report");
  assert.equal(relayAt(23).flash, 1);
  assert.equal(relayAt(28).flash, 1);
});
test("relay flights reach both partners and keep their start and end speeds smooth", () => {
  const a = { x: 800, y: 400 },
    b = { x: 400, y: 400 };
  assert.deepEqual(flight(a, b, 0, 90), a);
  const end = flight(a, b, 1, 90);
  assert.ok(Math.abs(end.x - b.x) < 1e-8 && Math.abs(end.y - b.y) < 1e-8);
  assert.ok(flight(a, b, 0.5, 90).y < 400);
  assert.ok(Math.abs(flight(a, b, 0.001, 90).x - a.x) < 0.001);
});
test("handoff gives both partners time and resumes the existing story continuously", () => {
  assert.equal(contentTimeAt(21.5), 9.5);
  assert.equal(contentTimeAt(24), 9.5);
  assert.equal(contentTimeAt(27.5), 9.5);
  assert.equal(contentTimeAt(49), 31);
  for (let t = 20; t < 30; t += 0.01)
    assert.ok(contentTimeAt(t + 0.01) >= contentTimeAt(t));
});
