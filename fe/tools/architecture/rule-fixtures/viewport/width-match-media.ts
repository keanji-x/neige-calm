// Branch (b) alone: the width query is written out by hand, so no import of the
// shared constant exists for branch (a) to catch. Deleting branch (a) leaves
// this fixture green.
export function isCompact(): boolean {
  return globalThis.matchMedia('(width < 60rem)').matches;
}
