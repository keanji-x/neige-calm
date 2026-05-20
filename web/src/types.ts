// Calm UI types — Cove (project) / Wave (task) / Today (home).
// Mirrors the design's seed data shape; renamed Sea → Cove.

/**
 * Wave status — kernel itself stores no status; plugins write it via overlays.
 * - `idle`    : no plugin has set status (the default). Visually calm.
 * - `waiting` : an overlay explicitly says the wave is waiting on the user.
 *               Only this surfaces in the sidebar's "Waiting on you" group.
 * - `running` : an overlay explicitly says work is in flight (renders the
 *               progress bar + pulse).
 */
export type WaveStatus = 'idle' | 'running' | 'waiting';

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
}

/**
 * Codex (OpenAI) agent card. The kernel doesn't persist hook stream state
 * — `CodexCard` subscribes to `card:<id>` on the WS bus and renders the
 * live event stream. The card payload only carries the spawn params for
 * diagnostics / replay; `initial_prompt` is preserved so the user can
 * see what the agent was started with.
 */
export interface CodexCardData {
  type: 'codex';
  id?: string;
  initialPrompt: string;
  model?: string;
  cwd?: string;
}

/**
 * Transient "config card" — an in-flight AddPanel selection that hasn't
 * been submitted yet. Lives only in the Wave page's local state; never
 * persisted, never reaches the kernel. The `card-create` flow swaps it
 * for the real card once the user submits the SchemaForm.
 */
export interface ConfigCardData {
  type: 'config';
  id?: string;
  /** Kind being configured — used by the renderer to look up the schema
   *  from the registry. */
  targetKind: string;
}

export type WaveCardData =
  | TerminalCardData
  | PluginCardData
  | CodexCardData
  | ConfigCardData;

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
  progress: number;
  eta: string;
  now: string;
  cards?: WaveCardSlot[];
}

export type Route =
  | { name: 'today' }
  | { name: 'cove'; coveId: string }
  | { name: 'wave'; id: string };
