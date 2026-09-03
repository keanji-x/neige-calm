// Track lifecycle helpers — single source of truth for "what bucket does
// this track belong to right now?". Used by Sidebar's "Waiting on you"
// section, Today's running/waiting counters, Area's bucket sort, and the
// row/glyph/progress-bar treatment on TrackRow / TrackGlyph.
//
// The vocabulary mirrors the Rust `TrackLifecycle` enum
// (`crates/calm-server/src/model.rs`). Two derived predicates capture
// the two grouping concerns we surface today:
//
//   * `isWaitingForUser` — the track needs human attention (blocked,
//     reviewing, failed). These bubble to the top of the sidebar list.
//   * `isRunning`        — the track has work in flight (planning,
//     dispatching, working). Rendered with a live pulse + progress bar.
//
// `done` / `draft` / `canceled` fall through both checks; the UI treats
// them as quiet structural rows.

import type { Track, TrackLifecycle } from '../types';

export const isWaitingForUser = (l: TrackLifecycle): boolean =>
  l === 'blocked' || l === 'reviewing' || l === 'failed';

export const isRunning = (l: TrackLifecycle): boolean =>
  l === 'planning' || l === 'dispatching' || l === 'working';

export const lifecycleRank = (w: Track): number => {
  if (isWaitingForUser(w.lifecycle)) return 0;
  if (isRunning(w.lifecycle)) return 1;
  return 2;
};

export const sortByLifecycleRank = (tracks: readonly Track[]): Track[] =>
  [...tracks].sort((a, b) => lifecycleRank(a) - lifecycleRank(b));

/**
 * UI grouping predicate for "Waiting on you" surfaces (sidebar section,
 * Today header counter, calendar event highlight). ORs the
 * lifecycle-derived bucket with the kernel `card_fsm`-derived
 * `anyCardNeedsInput` signal so the user sees tracks where Planner Agent
 * hasn't (yet) driven `working → blocked` but a worker card is sitting
 * on an `AwaitingInput`/`Errored` hook.
 *
 * Lives at the UI layer, NOT inside `isWaitingForUser`, because the two
 * signals have different ownership (Planner Agent vs. kernel) and
 * different storage (column vs. overlay) — keeping the OR here means
 * the pure-lifecycle predicate stays usable for places that genuinely
 * want the lifecycle bucket (e.g. Area's bucket sort, the lifecycle
 * badge). See issue #254.
 */
export const trackNeedsUserAttention = (w: Track): boolean =>
  isWaitingForUser(w.lifecycle) || w.anyCardNeedsInput;
