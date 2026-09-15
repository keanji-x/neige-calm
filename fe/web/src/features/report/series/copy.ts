/**
 * The series figure's status line, declared once (#1693).
 *
 * The figcaption states three facts the kernel resolved about the row —
 * whether the author wrote `as_of`, whether the source has published past
 * it, and the earliest date the data is complete through — and each fact
 * gets its own words. "not pinned" is retired: a frozen block whose last
 * day has not arrived yet was read as "the author did not freeze this",
 * when the truth is that the author did and the data has simply not
 * reached that day. The three sentences live here, keyed by the state the
 * figure is in, so `public.tsx` chooses a state and never composes prose.
 */

export const SERIES_STATUS_COPY = Object.freeze({
  /** `as_of` written and the source has published past it: immutable now. */
  pinned: (asOf: string) => `frozen at ${asOf} · pinned`,
  /** `as_of` written, the source has not published past it yet: the day is coming. */
  pending: (asOf: string, through: string) => `frozen at ${asOf} · complete through ${through} · pin pending`,
  /** No `as_of`: the block follows the latest complete trading day. */
  live: (through: string, resolvedAt: string) => `live · complete through ${through} · resolved ${resolvedAt}`,
  /** `complete_through` when no asset in the row resolved. */
  throughUnknown: 'unknown',
} as const);
