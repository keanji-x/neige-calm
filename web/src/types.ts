// Calm UI types — Cove (project) / Wave (task) / Today (home).
// Mirrors the design's seed data shape; renamed Sea → Cove.

/**
 * Wave status — kernel itself stores no status; plugins write it via overlays.
 * - `idle`    : no plugin has set status (the default). Visually calm.
 * - `waiting` : an overlay explicitly says the wave is waiting on the user.
 *               Only this surfaces in the sidebar's "Waiting on you" group.
 * - `running` : an overlay explicitly says work is in flight (renders the
 *               progress bar + pulse).
 *
 * This 3-state vocabulary stays the canonical Wave summary that the legacy
 * grouping (Sidebar / Today / Cove filters) reads. The per-card FSM
 * (`web/src/cards/builtins/codex.tsx`) writes 6-state values via the
 * `card_fsm` task — they're projected down to this enum in `adaptWave`. The
 * full FSM name and counts ride along on `Wave.fsmState` / `Wave.counts`
 * for the new dot + badge UI that wants the richer surface.
 */
export type WaveStatus = 'idle' | 'running' | 'waiting';

/**
 * 6-state per-card / per-wave FSM (see `crates/calm-server/src/card_fsm.rs`).
 * Wire names are PascalCase — kept identical between Rust and TS so a state
 * string round-trips through overlays unchanged.
 */
export type FsmState =
  | 'Starting'
  | 'Idle'
  | 'Working'
  | 'AwaitingInput'
  | 'Errored'
  | 'Done';

/** Wave-level FSM payload `counts` block (only present on wave overlays). */
export interface FsmCounts {
  working: number;
  awaiting: number;
  errored: number;
}

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
}

/**
 * Plugin-provided iframe card. The kernel card kind is the canonical MCP Apps
 * resource URI `ui://<plugin_id>/<view_id>`. The legacy Neige-dialect form
 * `plugin:<plugin_id>:<view_id>` was deleted in M4 — the only consumer
 * (hello-world) is rewritten in M6.
 *
 * `plugin_id` and `view_id` are not stored on the card; derive them lazily at
 * use sites via `parsePluginCardKind(resource_uri)` from `cards/plugin-iframe`.
 */
export interface PluginCardData {
  type: 'plugin';
  id?: string;
  /** Full `ui://<plugin_id>/<view_id>` URI. */
  resource_uri: string;
  /** Display title for the card head. Sourced from `payload.title` set by
   *  the plugin's `tools/call` result. Falls back to the view_id when the
   *  plugin didn't provide one. */
  title?: string;
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
}

export type WaveCardData =
  | TerminalCardData
  | PluginCardData
  | CodexCardData;

/**
 * A position in a Wave's card grid. Either a parsed UI card (the happy
 * path) or an "unknown" placeholder that the registry's `adaptKernelCard`
 * couldn't claim — typically because the kernel card's payload failed its
 * per-kind zod schema. We keep this slot type separate from `WaveCardData`
 * so the discriminated union stays clean: every `WaveCardData` is a card
 * we know how to render, and the fallback path lives one layer up.
 */
export type WaveCardSlot =
  | { kind: 'card'; card: WaveCardData }
  | { kind: 'unknown'; id: string; kernelKind: string };

export interface Wave {
  id: string;
  coveId: string;
  title: string;
  status: WaveStatus;
  /**
   * Richer FSM state for the new per-card-FSM-driven dot/badge UI. Optional
   * because legacy plugin overlays still use the 3-state `status` field, and
   * cards that aren't tracked by the kernel FSM (terminal, plugin — phase 2)
   * leave this unset.
   */
  fsmState?: FsmState;
  /** Per-state card counts inside this wave (only set when fsmState is set). */
  counts?: FsmCounts;
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
