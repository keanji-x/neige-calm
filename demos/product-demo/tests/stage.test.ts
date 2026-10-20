import test from "node:test";
import assert from "node:assert/strict";
import spec from "../live/spec.json" with { type: "json" };
import { validateStage, sceneCamera } from "../src/scene-motion.ts";
import type { SceneSpec } from "../src/scene-model.ts";

test("the complete stage keeps physical speakers and their dialogue in frame", () => {
  assert.equal(validateStage(spec as SceneSpec), true);
});
test("camera keys do not snap position or scale", () => {
  for (const at of spec.camera.zoom.slice(1).map((k) => k.at)) {
    const a = sceneCamera(spec as SceneSpec, at - 1e-5),
      b = sceneCamera(spec as SceneSpec, at + 1e-5);
    assert.ok(Math.abs(a.scale - b.scale) < 0.001);
    assert.ok(Math.abs(a.x - b.x) < 0.001);
    assert.ok(Math.abs(a.y - b.y) < 0.001);
  }
});
test("speaking roles cannot be hidden or undefined", () => {
  const hidden = structuredClone(spec) as SceneSpec;
  hidden.participants[1].visibility.forEach((k) => (k.value = 0));
  assert.throws(() => validateStage(hidden), /Speaking participant is hidden/);
  const missing = structuredClone(spec) as SceneSpec;
  missing.dialogue[0].speaker = "missing";
  assert.throws(
    () => validateStage(missing),
    /Dialogue has no physical speaker/,
  );
});
