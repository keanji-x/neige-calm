import { expect, it } from 'vitest';
import { liftKeyframe, rebaseLift, type SurfaceFrame } from './geometry.ts';

const frame: SurfaceFrame = { document, size: { width: 120.5, height: 60.25 }, viewportFromLocal: { x: 100.25, y: 80.5, scaleX: 1.25, scaleY: 0.75 } };
it('projects a viewport source into each fractional local frame without clamping', () => {
  const paint = liftKeyframe(frame, { x: 40, y: 20 }, { progress: 0, x: 0, y: 0 });
  const matrix = String(paint.transform).match(/matrix\(([^)]+)\)/)![1].split(',').map(Number);
  // Independent camera oracle at depth 8, with the source outside this surface.
  const scale = 800 / 808;
  expect(matrix[0]).toBeCloseTo(scale, 10);
  expect(matrix[4]).toBeCloseTo((1 - scale) * (40 - 100.25) / 1.25, 10);
  expect(matrix[5]).toBeCloseTo(((1 - scale) * (20 - 80.5) + 0.56) / 0.75, 10);
});
it('rebases projection position and derivative through origin and frame changes', () => {
  const next = { ...frame, viewportFromLocal: { x: 140.75, y: 85, scaleX: 1.5, scaleY: 0.9 } };
  const from = { value: { progress: 0.3, x: 2, y: -3 }, velocity: { progress: 4, x: 12, y: -6 } };
  const oldOrigin = { x: 40, y: 20 }; const newOrigin = { x: 90, y: 30 };
  const rebased = rebaseLift(from, frame, oldOrigin, next, newOrigin);
  const translation = (f: SurfaceFrame, a: { x: number; y: number }, state: typeof from.value) => String(liftKeyframe(f, a, state).transform).match(/matrix\(([^)]+)\)/)![1].split(',').map(Number).slice(4);
  expect(translation(next, newOrigin, rebased.value)[0]).toBeCloseTo(translation(frame, oldOrigin, from.value)[0], 10);
  expect(translation(next, newOrigin, rebased.value)[1]).toBeCloseTo(translation(frame, oldOrigin, from.value)[1], 10);
  const dt = 0.000001;
  const advance = (s: typeof from) => ({ progress: s.value.progress + dt * s.velocity.progress, x: s.value.x + dt * s.velocity.x, y: s.value.y + dt * s.velocity.y });
  const old = translation(frame, oldOrigin, from.value); const updated = translation(next, newOrigin, rebased.value);
  for (const axis of [0, 1]) expect((translation(next, newOrigin, advance(rebased))[axis] - updated[axis]) / dt)
    .toBeCloseTo((translation(frame, oldOrigin, advance(from))[axis] - old[axis]) / dt, 4);
});
