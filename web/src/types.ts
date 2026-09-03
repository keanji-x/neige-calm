// Calm UI types — Area (project) / Track (task) / Today (home).
// Mirrors the design's seed data shape; renamed Sea → Area.

/**
 * Issue #145 — Track lifecycle state machine.
 *
 * Mirrors the Rust `TrackLifecycle` enum (`crates/calm-server/src/model.rs`)
 * and the ts-rs-emitted union in `api/generated-events.ts`. Keep this
 * vocabulary 1:1 with the kernel; the Planner Agent drives the happy path
 * (`draft → planning → dispatching → working → reviewing → done`) and the
 * UI projects it as a badge on the Track header / row.
 *
 * `archived` is intentionally NOT a lifecycle state — archive is
 * orthogonal visibility/history management on `Track.archived_at`.
 */
export type TrackLifecycle =
  | 'draft'
  | 'planning'
  | 'dispatching'
  | 'working'
  | 'blocked'
  | 'reviewing'
  | 'done'
  | 'canceled'
  | 'failed';

/**
 * 6-state per-card FSM (see `crates/calm-server/src/card_fsm.rs`). Wire
 * names are PascalCase — kept identical between Rust and TS so a state
 * string round-trips through overlays unchanged.
 *
 * Track-level state is owned by `TrackLifecycle` (above); per-card status
 * dots on the codex card head still consume this enum directly via
 * `web/src/cards/builtins/codex.tsx`.
 */
export type FsmState =
  | 'Starting'
  | 'Idle'
  | 'Working'
  | 'AwaitingInput'
  | 'Errored'
  | 'Done';

export interface Area {
  id: string;
  name: string;
  subtitle: string;
  color: string;
}

export type TermLineKind =
  | 'log'
  | 'cmd'
  | 'out'
  | 'edit'
  | 'err'
  | 'me'
  | 'ask'
  | 'hint'
  | 'pass'
  | 'fail';

export interface TermLine {
  kind: TermLineKind;
  text: string;
}

export interface TrackCardDataMap {
  // Card entries self-register their data shape through module augmentation.
  // Add new card kinds in their entry module; this central type stays open.
}

export type TrackCardData = TrackCardDataMap[keyof TrackCardDataMap];

/**
 * A position in a Track's card grid. Either a parsed UI card (the happy
 * path) or an "unknown" placeholder that the registry's `adaptKernelCard`
 * couldn't claim — typically because the kernel card's payload failed its
 * per-kind zod schema. We keep this slot type separate from `TrackCardData`
 * so the discriminated union stays clean: every `TrackCardData` is a card
 * we know how to render, and the fallback path lives one layer up.
 *
 * `sort` mirrors the kernel `Card.sort` value. It's plumbed through so the
 * list view (Slice 9 of issue #56) can compute a new `sort` for the swap
 * mutation when the user presses Alt+ArrowUp/Down. Optional so older code
 * paths constructing a slot in tests don't have to fabricate one.
 */
export type TrackCardSlot =
  | {
      kind: 'card';
      card: TrackCardData;
      sort?: number;
      /**
       * Issue #229 PR A — kernel-owned cards (planner today; track-report in
       * PR B) carry `deletable: false` on the kernel `Card` row. The
       * server's `DELETE /api/cards/:id` rejects with 403 in that case;
       * the UI mirrors the same policy by suppressing the X close
       * affordance on the card head. Optional so existing tests /
       * legacy code paths constructing a slot without a kernel reference
       * default to "user-deletable" (matches the migration's DB
       * DEFAULT of 1).
       */
      deletable?: boolean;
    }
  | { kind: 'unknown'; id: string; kernelKind: string; sort?: number; deletable?: boolean };

export interface Track {
  id: string;
  areaId: string;
  title: string;
  /**
   * Issue #145 — explicit lifecycle stamped by the kernel. Required: every
   * kernel-shaped track carries one (defaulted to `'draft'` server-side).
   * This is the single source of truth for track-level state — Sidebar's
   * "Waiting on you", Today's running/waiting counters, Area's bucket
   * sort, and the row/header status pill all derive from it via
   * `shared/lifecycle.ts`. The Planner Agent writes it explicitly; nothing
   * else in the codebase should re-derive it.
   */
  lifecycle: TrackLifecycle;
  /**
   * Issue #254 — `true` when any card under this track is in
   * `AwaitingInput` or `Errored`. Derived from the track-scoped
   * `any_card_needs_input` overlay written by `card_fsm`. Required (not
   * optional) — the adapter defaults it to `false` when the overlay is
   * absent, matching the [[required-over-option]] convention so a
   * forgotten field surfaces as a type error rather than silent
   * `undefined`.
   *
   * Pairs with `lifecycle` at the sidebar "Waiting on you" filter
   * (`trackNeedsUserAttention` in `shared/lifecycle.ts`): the two signals
   * are orthogonal (Planner Agent owns lifecycle, card_fsm owns this) and
   * OR'd together at the UI layer.
   */
  anyCardNeedsInput: boolean;
  progress: number;
  eta: string;
  now: string;
  /**
   * Issue #250 PR 5 — track creation timestamp (unix ms), as stamped by
   * the kernel. Surfaced on the UI shape (not just the wire shape) so
   * Today's CalendarCard can render track activity per day without
   * re-pulling the raw kernel row. Required by reality — every kernel
   * `Track` carries a `created_at` — and required at the UI layer per
   * [[required-over-option]] so a forgotten field is a compile error
   * rather than a calendar that silently treats every track as new today.
   */
  createdAt: number;
  /**
   * Issue #250 PR 5 — track terminal timestamp (unix ms), `null` while
   * the track is still open. Pairs with `createdAt` for the calendar's
   * "active on day D" predicate (`createdAt ≤ end-of-day AND
   * (terminalAt == null OR terminalAt ≥ start-of-day)`). Required (not
   * optional) — the adapter writes `null` explicitly when the kernel
   * row has no terminal — so a forgotten branch surfaces as a type
   * error rather than silently dropping open tracks from the agenda.
   */
  terminalAt: number | null;
  /**
   * Unix-ms timestamp when the track was pinned, `null` when unpinned.
   * `null` rather than optional so a missing field from old wire payloads
   * surfaces as a type error rather than silent `undefined` in sort/filter
   * logic.
   */
  pinnedAt: number | null;
  cards?: TrackCardSlot[];
}

export type Route =
  | { name: 'today' }
  | { name: 'area'; areaId: string }
  | { name: 'track'; id: string }
  | { name: 'settings' };
