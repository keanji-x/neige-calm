// The documented escape: a non-literal argument is not analysed. Recorded as a
// fixture so the hole is a stated, tested property rather than a surprise.
const query = '(width < 60rem)';

export function isCompact(): boolean {
  return globalThis.matchMedia(query).matches;
}
