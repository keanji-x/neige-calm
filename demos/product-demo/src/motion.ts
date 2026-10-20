import type { Knot } from "./model.ts";
export const clamp = (x: number, a = 0, b = 1) => Math.max(a, Math.min(b, x));
export const ease = (u: number) => {
  const t = clamp(u);
  return t * t * t * (10 + t * (-15 + 6 * t));
};
export function sample(keys: Knot[], t: number): number {
  if (t <= keys[0].at) return keys[0].value;
  for (let i = 1; i < keys.length; i++)
    if (t <= keys[i].at) {
      const a = keys[i - 1],
        b = keys[i];
      return a.value + (b.value - a.value) * ease((t - a.at) / (b.at - a.at));
    }
  return keys[keys.length - 1].value;
}
