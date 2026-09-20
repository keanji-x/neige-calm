/** The series figure's status line, declared once: `public.tsx` chooses a state and never composes prose. */

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
