import mark from './neige-mark.svg?raw';

export type Point = readonly [number, number];

/** Read the released mark rather than maintaining a second drawing of its three folds. */
export function snowFold(index: number): readonly Point[] {
  const path = /<path d="([^"]+)"/.exec(mark);
  if (path === null) throw new Error('Neige mark has no canonical path');
  const coordinates = path[1].match(/-?\d+(?:\.\d+)?/g)?.map(Number);
  if (coordinates === undefined || coordinates.length !== 6) throw new Error('Neige mark must have three vertices');
  const angle = index * 120 * Math.PI / 180;
  return Array.from({ length: 3 }, (_, vertex): Point => {
    const x = coordinates[vertex * 2] - 192;
    const y = coordinates[vertex * 2 + 1] - 192;
    return [192 + x * Math.cos(angle) - y * Math.sin(angle), 192 + x * Math.sin(angle) + y * Math.cos(angle)];
  });
}

export function pointsOf(points: readonly Point[]): string {
  return points.map(([x, y]) => `${x.toFixed(3)},${y.toFixed(3)}`).join(' ');
}

export function foldCenter(points: readonly Point[]): Point {
  return [points.reduce((sum, [x]) => sum + x, 0) / 3, points.reduce((sum, [, y]) => sum + y, 0) / 3];
}

/** One chevron separates into three folds; both legs retain their lineage across the handoff. */
export function executionFold(index: number, amount: number): readonly Point[] {
  const lerp = (a: number, b: number) => a + (b - a) * amount;
  const snow = snowFold(index);
  const corner: Point = [lerp(172, snow[1][0]), lerp(192, snow[1][1])];
  const upperTarget = index === 2 ? -210 : -90 + index * 120;
  const lowerTarget = index === 2 ? 90 : 210 + index * 120;
  const upper = lerp(-135, upperTarget) * Math.PI / 180;
  const lower = lerp(135, lowerTarget) * Math.PI / 180;
  const first = lerp(Math.hypot(64, 64), Math.hypot(snow[0][0] - snow[1][0], snow[0][1] - snow[1][1]));
  const last = lerp(Math.hypot(64, 64), Math.hypot(snow[2][0] - snow[1][0], snow[2][1] - snow[1][1]));
  return [[corner[0] + first * Math.cos(upper), corner[1] + first * Math.sin(upper)], corner,
    [corner[0] + last * Math.cos(lower), corner[1] + last * Math.sin(lower)]];
}

/** Sample the approved polar motion. SVG interpolates the samples without a per-frame JS loop. */
export function executionFrames(index: number): Readonly<{ values: string; times: string }> {
  const frames: string[] = [pointsOf(executionFold(index, 0))];
  const times: number[] = [0];
  for (let step = 0; step <= 16; step++) {
    const t = step / 16;
    frames.push(pointsOf(executionFold(index, t * t * (3 - 2 * t))));
    times.push(.18 + .26 * t);
  }
  frames.push(pointsOf(executionFold(index, 1)));
  times.push(.56);
  for (let step = 1; step <= 16; step++) {
    const t = 1 - step / 16;
    frames.push(pointsOf(executionFold(index, t * t * (3 - 2 * t))));
    times.push(.56 + .26 * step / 16);
  }
  frames.push(pointsOf(executionFold(index, 0)));
  times.push(1);
  return { values: frames.join(';'), times: times.map(t => t.toFixed(5)).join(';') };
}
