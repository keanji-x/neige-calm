// Wave lifecycle helpers — single source of truth for "what bucket does
// this wave belong to right now?". Used by Sidebar's "Waiting on you"
// section, Today's running/waiting counters, Cove's bucket sort, and the
// row/glyph/progress-bar treatment on WaveRow / WaveGlyph.
//
// The vocabulary mirrors the Rust `WaveLifecycle` enum
// (`crates/calm-server/src/model.rs`). Two derived predicates capture
// the two grouping concerns we surface today:
//
//   * `isWaitingForUser` — the wave needs human attention (blocked,
//     reviewing, failed). These bubble to the top of the sidebar list.
//   * `isRunning`        — the wave has work in flight (planning,
//     dispatching, working). Rendered with a live pulse + progress bar.
//
// `done` / `draft` / `canceled` fall through both checks; the UI treats
// them as quiet structural rows.

import type { Wave, WaveLifecycle } from '../types';

export const isWaitingForUser = (l: WaveLifecycle): boolean =>
  l === 'blocked' || l === 'reviewing' || l === 'failed';

export const isRunning = (l: WaveLifecycle): boolean =>
  l === 'planning' || l === 'dispatching' || l === 'working';

export const lifecycleRank = (w: Wave): number => {
  if (isWaitingForUser(w.lifecycle)) return 0;
  if (isRunning(w.lifecycle)) return 1;
  return 2;
};

export const sortByLifecycleRank = (waves: readonly Wave[]): Wave[] =>
  [...waves].sort((a, b) => lifecycleRank(a) - lifecycleRank(b));

/**
 * UI grouping predicate for "Waiting on you" surfaces (sidebar section,
 * Today header counter, calendar event highlight). ORs the
 * lifecycle-derived bucket with the kernel `card_fsm`-derived
 * `anyCardNeedsInput` signal so the user sees waves where Spec Agent
 * hasn't (yet) driven `working → blocked` but a worker card is sitting
 * on an `AwaitingInput`/`Errored` hook.
 *
 * Lives at the UI layer, NOT inside `isWaitingForUser`, because the two
 * signals have different ownership (Spec Agent vs. kernel) and
 * different storage (column vs. overlay) — keeping the OR here means
 * the pure-lifecycle predicate stays usable for places that genuinely
 * want the lifecycle bucket (e.g. Cove's bucket sort, the lifecycle
 * badge). See issue #254.
 */
export const waveNeedsUserAttention = (w: Wave): boolean =>
  isWaitingForUser(w.lifecycle) || w.anyCardNeedsInput;
