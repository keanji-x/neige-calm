// Branch (a) alone: the shared breakpoint constant is imported outside
// `ui/viewport`. Nothing here calls `matchMedia`, so deleting branch (b) leaves
// this fixture green and only branch (a) can account for its one violation.
import { RAIL_COLLAPSE_QUERY } from './styles/breakpoints.ts';

export function describeLayout(): string {
  return RAIL_COLLAPSE_QUERY;
}
