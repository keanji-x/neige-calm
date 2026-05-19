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

export interface DocCardData {
  type: 'doc';
  id?: string;
  title: string;
  body: string;
}

export interface GitCommit {
  sha: string;
  msg: string;
  when: string;
}

export interface GitCardData {
  type: 'git';
  id?: string;
  branch: string;
  commits: GitCommit[];
}

export type DiffLineKind = 'ctx' | 'add' | 'rm';

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
}

export interface DiffHunk {
  header: string;
  lines: DiffLine[];
}

export interface DiffCardData {
  type: 'diff';
  id?: string;
  file: string;
  added: number;
  removed: number;
  hunks: DiffHunk[];
}

export interface PlanStep {
  label: string;
  done?: boolean;
  cur?: boolean;
  when?: string;
}

export interface PlanCardData {
  type: 'plan';
  id?: string;
  steps: PlanStep[];
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

export type WaveCardData =
  | TerminalCardData
  | DocCardData
  | GitCardData
  | DiffCardData
  | PlanCardData
  | PluginCardData;

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
  plan?: PlanStep[];
  cards?: WaveCardSlot[];
}

export type Route =
  | { name: 'today' }
  | { name: 'cove'; coveId: string }
  | { name: 'wave'; id: string };
