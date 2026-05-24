// Calm UI types — Cove (project) / Wave (task) / Today (home).
// Mirrors the design's seed data shape; renamed Sea → Cove.

/**
 * Issue #145 — Wave lifecycle state machine.
 *
 * Mirrors the Rust `WaveLifecycle` enum (`crates/calm-server/src/model.rs`)
 * and the ts-rs-emitted union in `api/generated-events.ts`. Keep this
 * vocabulary 1:1 with the kernel; the Spec Agent drives the happy path
 * (`draft → planning → dispatching → working → reviewing → done`) and the
 * UI projects it as a badge on the Wave header / row.
 *
 * `archived` is intentionally NOT a lifecycle state — archive is
 * orthogonal visibility/history management on `Wave.archived_at`.
 */
export type WaveLifecycle =
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
 * Wave-level state is owned by `WaveLifecycle` (above); per-card status
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

export interface Cove {
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

export interface TerminalCardData {
  type: 'terminal';
  // Kernel `Card.id`. Stable per card across reorders — used as the RGL key
  // and the lookup for the per-card layout entry in localStorage.
  id?: string;
  title: string;
  lines: TermLine[];
  // Optional pointer at a kernel Terminal row (calm-server's
  // `Terminal.id`). When set, the card hosts a live xterm/PTY rather than
  // rendering the static `lines`.
  terminalId?: string;
  // When the payload's `schemaVersion` is newer than what this build of
  // the frontend understands, the adapter still produces a card so the
  // grid layout doesn't collapse — but the component renders a fallback
  // pointing the user at refresh. See Tier A upgrade-stability policy.
  unsupportedVersion?: number;
}

/**
 * Plugin-provided iframe card. The kernel card kind is the canonical MCP Apps
 * resource URI `ui://<plugin_id>/<view_id>`. The legacy Neige-dialect form
 * `plugin:<plugin_id>:<view_id>` was deleted in M4; the hello-world demo
 * (its last consumer) was deleted alongside the WaveLifecycle unification.
 *
 * `plugin_id` and `view_id` are not stored on the card; derive them lazily at
 * use sites via `parsePluginCardKind(resource_uri)` from `cards/plugin-iframe`.
 */
export interface PluginCardData {
  type: 'plugin';
  id?: string;
  /** Full `ui://<plugin_id>/<view_id>` URI. */
  resource_uri: string;
}

/**
 * Codex (OpenAI) agent card. Interactive variant: the kernel binds a
 * `calm-session-daemon` PTY running `codex` to this card and stamps the
 * `terminal_id` into the payload. `CodexCard` then renders the live TUI
 * via `XtermView` and overlays a status bar fed from `codex.hook` events
 * on the WS bus.
 *
 * Older cards created before the interactive rewrite may not have a
 * `terminalId` yet — the card renders an "agent is starting" placeholder
 * in that case.
 */
export interface CodexCardData {
  type: 'codex';
  id?: string;
  /** Optional pointer at the PTY row spawned for this card. */
  terminalId?: string;
  cwd?: string;
  /** See `TerminalCardData.unsupportedVersion`. */
  unsupportedVersion?: number;
}

/**
 * Wave report card payload — issue #229.
 *
 * Kernel-owned card minted at wave-create time. One per wave. The
 * payload is a single Markdown body plus a one-line summary; the
 * frontend derives sections by splitting at H1 (`^# `) headings at
 * render time. See `web/src/cards/builtins/wave-report.tsx`.
 */
export interface WaveReportCardData {
  type: 'wave-report';
  id?: string;
  /** One-line preview used by sidebars / wave list rows. */
  summary: string;
  /** Markdown source — rendered as collapsible sections in the card. */
  body: string;
  /** See `TerminalCardData.unsupportedVersion`. */
  unsupportedVersion?: number;
}

export type WaveCardData =
  | TerminalCardData
  | PluginCardData
  | CodexCardData
  | WaveReportCardData;

/**
 * A position in a Wave's card grid. Either a parsed UI card (the happy
 * path) or an "unknown" placeholder that the registry's `adaptKernelCard`
 * couldn't claim — typically because the kernel card's payload failed its
 * per-kind zod schema. We keep this slot type separate from `WaveCardData`
 * so the discriminated union stays clean: every `WaveCardData` is a card
 * we know how to render, and the fallback path lives one layer up.
 *
 * `sort` mirrors the kernel `Card.sort` value. It's plumbed through so the
 * list view (Slice 9 of issue #56) can compute a new `sort` for the swap
 * mutation when the user presses Alt+ArrowUp/Down. Optional so older code
 * paths constructing a slot in tests don't have to fabricate one.
 */
export type WaveCardSlot =
  | {
      kind: 'card';
      card: WaveCardData;
      sort?: number;
      /**
       * Issue #229 PR A — kernel-owned cards (spec today; wave-report in
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

export interface Wave {
  id: string;
  coveId: string;
  title: string;
  /**
   * Issue #145 — explicit lifecycle stamped by the kernel. Required: every
   * kernel-shaped wave carries one (defaulted to `'draft'` server-side).
   * This is the single source of truth for wave-level state — Sidebar's
   * "Waiting on you", Today's running/waiting counters, Cove's bucket
   * sort, and the row/header status pill all derive from it via
   * `shared/lifecycle.ts`. The Spec Agent writes it explicitly; nothing
   * else in the codebase should re-derive it.
   */
  lifecycle: WaveLifecycle;
  /**
   * Issue #254 — `true` when any card under this wave is in
   * `AwaitingInput` or `Errored`. Derived from the wave-scoped
   * `any_card_needs_input` overlay written by `card_fsm`. Required (not
   * optional) — the adapter defaults it to `false` when the overlay is
   * absent, matching the [[required-over-option]] convention so a
   * forgotten field surfaces as a type error rather than silent
   * `undefined`.
   *
   * Pairs with `lifecycle` at the sidebar "Waiting on you" filter
   * (`waveNeedsUserAttention` in `shared/lifecycle.ts`): the two signals
   * are orthogonal (Spec Agent owns lifecycle, card_fsm owns this) and
   * OR'd together at the UI layer.
   */
  anyCardNeedsInput: boolean;
  progress: number;
  eta: string;
  now: string;
  cards?: WaveCardSlot[];
}

export type Route =
  | { name: 'today' }
  | { name: 'cove'; coveId: string }
  | { name: 'wave'; id: string }
  | { name: 'settings' };
