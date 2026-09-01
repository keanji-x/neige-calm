// The eight legitimate `matchMedia` calls this rule must never reject:
// user-preference and pointer features carry no layout decision.
export function preferences(): readonly boolean[] {
  return [
    globalThis.matchMedia('(prefers-color-scheme: dark)').matches,
    globalThis.matchMedia('(prefers-reduced-motion: reduce)').matches,
    globalThis.matchMedia('(pointer: coarse)').matches,
    window.matchMedia(`(pointer: fine)`).matches,
  ];
}
