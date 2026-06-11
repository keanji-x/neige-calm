// REST client for calm-server. One function per kernel route; thin wrapper
// over `fetch` that throws on non-2xx with the server's `{error, code}` body.

import { fireUnauthorized } from './onUnauthorized';
import type { paths } from './generated';
import type { HarnessItem } from './generated-events';
import type {
  CardPatchBody,
  CovePatchBody,
  CoveResolveBody,
  KernelCard,
  KernelCove,
  KernelOverlay,
  KernelTerminal,
  KernelWave,
  KernelWaveDetail,
  GitDiffResponse,
  GitStatusResponse,
  ListdirResponse,
  NewCardBody,
  NewClaudeCardBody,
  NewCodexCardBody,
  NewCoveBody,
  NewOverlayBody,
  NewTerminalCardBody,
  NewWaveBody,
  ReadFileResponse,
  SettingsBag,
  SettingsPutBody,
  WavePatchBody,
} from './wire';

export type WaveFsEntry =
  paths['/api/waves/{id}/files/ls']['get']['responses'][200]['content']['application/json'][number];
export type WaveFsContent =
  paths['/api/waves/{id}/files/cat']['get']['responses'][200]['content']['application/json'];

export class CalmApiError extends Error {
  status: number;
  code: string;
  /**
   * Raw parsed JSON body, when the server returned one. Most routes
   * surface errors as `{error, code}` and the request helper hoists
   * those into `.message` / `.code` — `.body` is the escape hatch for
   * routes that return a typed body directly (e.g. `POST /api/waves`
   * 409 → `FolderConflict { conflict_path, conflict_kind, ... }` per
   * `routes::waves` issue #250 PR 2). Callers that care about the
   * structured shape (NewTaskForm) type-narrow this themselves; the
   * field stays `unknown` here so the wire types don't leak into
   * every consumer.
   */
  body: unknown;
  constructor(status: number, code: string, msg: string, body?: unknown) {
    super(msg);
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const init: RequestInit = {
    method,
    credentials: 'include',
    headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  };
  const res = await fetch(path, init);
  if (res.status === 204) return undefined as T;
  if (!res.ok) {
    let code = 'http_error';
    let msg = res.statusText;
    let parsedBody: unknown = undefined;
    try {
      const j = await res.json();
      parsedBody = j;
      if (typeof j?.code === 'string') code = j.code;
      if (typeof j?.error === 'string') msg = j.error;
    } catch {
      /* body wasn't json — keep the status text */
    }
    // Issue #189 — any 401 means our session is gone (cookie expired,
    // server-side restart, owner logged out from a sibling tab). Flag
    // it via the global `onUnauthorized` channel so the SessionProvider
    // can wipe state + bounce back to LoginPage. We still throw the
    // error so the caller's mutation/query reports the failure cleanly;
    // the SessionProvider's cleanup runs in a microtask, decoupled from
    // the unwind.
    if (res.status === 401) {
      fireUnauthorized();
    }
    throw new CalmApiError(res.status, code, msg, parsedBody);
  }
  // 200 / 201 with body
  return (await res.json()) as T;
}

// ---------------- coves ----------------

export const listCoves = () =>
  request<KernelCove[]>('GET', '/api/coves');
export const createCove = (b: NewCoveBody) =>
  request<KernelCove>('POST', '/api/coves', b);

/**
 * Issue #175 — idempotent upsert for the singleton system cove that hosts
 * the default Today terminal's wave + card. Returns the existing row when
 * one is present (200), otherwise mints a fresh row (201). The
 * `useTodayTerminal` hook calls this on bootstrap so the user's sidebar
 * never sees the underlying scaffolding cove.
 *
 * The server-side `POST /api/coves/system` handler enforces the
 * at-most-one invariant via a partial unique index on
 * `coves(kind) WHERE kind='system'` (migration 0009) — two tabs racing
 * this call are both safe.
 */
export const getOrCreateSystemCove = () =>
  request<KernelCove>('POST', '/api/coves/system');
export const updateCove = (id: string, b: CovePatchBody) =>
  request<KernelCove>('PATCH', `/api/coves/${encodeURIComponent(id)}`, b);
export const deleteCove = (id: string) =>
  request<void>('DELETE', `/api/coves/${encodeURIComponent(id)}`);
export const wavesInCove = (coveId: string) =>
  request<KernelWave[]>('GET', `/api/coves/${encodeURIComponent(coveId)}/waves`);

/**
 * Issue #250 PR 3 — longest-prefix lookup for "which cove (if any)
 * already claims this absolute path?". Returns `null` when no cove
 * covers it; the caller then either picks an existing cove + opts in
 * to `attach_folder: true` on the wave-create, or mints a fresh cove.
 *
 * NewTaskForm is the only consumer today — debounced cwd-input change
 * fires this lookup so the cove dropdown can lock to the auto-matched
 * cove (hit) or stay user-editable (miss). 400 means non-absolute path
 * and is treated as a "skip resolve" — the form already enforces the
 * absolute-path shape inline before submit.
 */
export const resolveCovePath = (path: string) =>
  request<CoveResolveBody | null>(
    'GET',
    `/api/coves/resolve?path=${encodeURIComponent(path)}`,
  );

// ---------------- waves ----------------

/**
 * Issue #250 PR 2 — calendar window query. `GET /api/waves?since=&until=&cove_id=`
 * returns every wave overlapping the window `[since, until]` (unix ms,
 * inclusive at both endpoints). The kernel applies the dual predicate
 * `created_at <= until AND (terminal_at IS NULL OR terminal_at >= since)`
 * so still-open waves remain visible across every day they span. All
 * three params are optional; omitting all degenerates to "every wave".
 *
 * Calendar (issue #250 PR 5) uses this as the one read for the weekly
 * grid: pass `since`/`until` for the current week's window in local
 * time, no `cove_id` so the result aggregates across coves.
 */
export const wavesRange = (params: {
  since?: number;
  until?: number;
  cove_id?: string;
}) => {
  const qs = new URLSearchParams();
  if (params.since !== undefined) qs.set('since', String(params.since));
  if (params.until !== undefined) qs.set('until', String(params.until));
  if (params.cove_id) qs.set('cove_id', params.cove_id);
  const tail = qs.toString();
  return request<KernelWave[]>('GET', `/api/waves${tail ? `?${tail}` : ''}`);
};

export const createWave = (b: NewWaveBody) =>
  request<KernelWave>('POST', '/api/waves', b);
export const getWaveDetail = (id: string) =>
  request<KernelWaveDetail>('GET', `/api/waves/${encodeURIComponent(id)}`);
export const updateWave = (id: string, b: WavePatchBody) =>
  request<KernelWave>('PATCH', `/api/waves/${encodeURIComponent(id)}`, b);
export const deleteWave = (id: string) =>
  request<void>('DELETE', `/api/waves/${encodeURIComponent(id)}`);

/**
 * Issue #247 PR3 — user-driven wave-report edit. The kernel persists the
 * `{summary, body}` pair through the same CRDT pipeline the spec agent's
 * `calm.report.write` MCP tool uses, then echoes back the projected
 * `WaveReportPayload` (with `schemaVersion` reasserted). Session-gated:
 * only `Actor::User` is accepted; worker / plugin / spec sessions are
 * rejected with 403 by construction (`author` is derived server-side,
 * never accepted on the wire — `serde(deny_unknown_fields)` closes the
 * spoofing path).
 *
 * The card UI calls this from its inline edit mode; on success it swaps
 * to the returned payload so the user sees the post-merge text without
 * waiting for the `card.updated` / `wave.report_edited` events to roll
 * back through the WS bus.
 */
export const updateWaveReport = (
  id: string,
  b: { summary: string; body: string },
) =>
  request<{ schemaVersion: number; summary: string; body: string }>(
    'POST',
    `/api/waves/${encodeURIComponent(id)}/report`,
    b,
  );

export const listWaveFiles = (waveId: string, path?: string | null) => {
  const qs = new URLSearchParams();
  if (path != null && path.length > 0) qs.set('path', path);
  const tail = qs.toString();
  return request<WaveFsEntry[]>(
    'GET',
    `/api/waves/${encodeURIComponent(waveId)}/files/ls${tail ? `?${tail}` : ''}`,
  );
};

export const catWaveFile = (waveId: string, path: string) => {
  const qs = new URLSearchParams({ path });
  return request<WaveFsContent>(
    'GET',
    `/api/waves/${encodeURIComponent(waveId)}/files/cat?${qs.toString()}`,
  );
};

// ---------------- cards ----------------

export const cardsInWave = (waveId: string) =>
  request<KernelCard[]>('GET', `/api/waves/${encodeURIComponent(waveId)}/cards`);
export const createCard = (waveId: string, b: NewCardBody) =>
  request<KernelCard>(
    'POST',
    `/api/waves/${encodeURIComponent(waveId)}/cards`,
    b,
  );
export const updateCard = (id: string, b: CardPatchBody) =>
  request<KernelCard>('PATCH', `/api/cards/${encodeURIComponent(id)}`, b);
export const deleteCard = (id: string) =>
  request<void>('DELETE', `/api/cards/${encodeURIComponent(id)}`);
export const resetSpecCard = (id: string) =>
  request<{ card_id: string; terminal_id: string; new_thread_id: string }>(
    'POST',
    `/api/cards/${encodeURIComponent(id)}/spec/reset`,
  );
export const sendSpecInput = (id: string, text: string) =>
  request<{ card_id: string; runtime_id: string }>(
    'POST',
    `/api/cards/${encodeURIComponent(id)}/spec/input`,
    { text },
  );
export const listHarnessItems = (
  id: string,
  params: {
    afterId?: number;
    limit?: number;
    direction?: 'asc' | 'desc';
  } = {},
) => {
  const qs = new URLSearchParams();
  if (params.afterId !== undefined) qs.set('after_id', String(params.afterId));
  if (params.limit !== undefined) qs.set('limit', String(params.limit));
  if (params.direction !== undefined) qs.set('direction', params.direction);
  const tail = qs.toString();
  return request<HarnessItem[]>(
    'GET',
    `/api/cards/${encodeURIComponent(id)}/harness/items${tail ? `?${tail}` : ''}`,
  );
};
export const restartClaudeCard = (id: string) =>
  request<KernelCard>(
    'POST',
    `/api/cards/${encodeURIComponent(id)}/claude/restart`,
  );

// ---------------- overlays ----------------

export const listOverlays = (
  entity_kind: 'wave' | 'card' | 'view',
  entity_id: string,
) =>
  request<KernelOverlay[]>(
    'GET',
    `/api/overlays?entity_kind=${entity_kind}&entity_id=${encodeURIComponent(entity_id)}`,
  );

/**
 * Lists every overlay of the given kind across the workspace. The kernel
 * extends `GET /api/overlays` to accept `entity_kind` alone (without
 * `entity_id`) for this use. The Sidebar uses the `'wave'` variant so
 * status indicators stay accurate without fanning out per-wave detail
 * fetches.
 */
export const listAllOverlays = (entity_kind: 'wave' | 'card') =>
  request<KernelOverlay[]>('GET', `/api/overlays?entity_kind=${entity_kind}`);

export const upsertOverlay = (b: NewOverlayBody) =>
  request<KernelOverlay>('POST', '/api/overlays', b);
export const deleteOverlay = (b: {
  plugin_id: string;
  entity_kind: string;
  entity_id: string;
  kind: string;
}) => request<void>('POST', '/api/overlays/delete', b);

// ---------------- terminals ----------------

/**
 * Atomic terminal-card create. Single round-trip writes the card row, its
 * linked terminal row, AND starts the terminal renderer. Server emits a
 * single `card.added` event carrying the final payload — no intermediate
 * `payload=null` flash for EventBridge to swallow. See `routes::terminal_cards`
 * and issue #13.
 *
 * 500 response means the daemon spawn failed; the persisted rows stay (the
 * orphan-terminal sweeper reaps them within ~60s).
 */
export const createTerminalCard = (waveId: string, b: NewTerminalCardBody) =>
  request<KernelCard>(
    'POST',
    `/api/waves/${encodeURIComponent(waveId)}/terminal-cards`,
    b,
  );

/** Look up the Terminal a card owns; 404s if the card has no terminal. */
export const getTerminalForCard = (cardId: string) =>
  request<KernelTerminal>(
    'GET',
    `/api/cards/${encodeURIComponent(cardId)}/terminal`,
  );

// ---------------- codex ----------------

/**
 * Atomic codex-card create (#117). Single round-trip writes the card row,
 * its linked terminal row, AND spawns the codex daemon. Server emits a
 * single `card.added` event carrying the final payload (with
 * `terminal_id` + optional `cwd`) — no intermediate `payload=null` flash,
 * no follow-up `card.updated`. Hook events still stream over the WS event
 * bus on `card:<card_id>` as `codex.hook` envelopes. See
 * `routes::codex_cards`.
 *
 * 500 response means the codex daemon spawn failed; the persisted rows
 * stay (the orphan-terminal sweeper reaps them within ~60s), matching
 * the terminal-card endpoint's contract.
 */
export const createCodexCard = (waveId: string, b: NewCodexCardBody) =>
  request<KernelCard>(
    'POST',
    `/api/waves/${encodeURIComponent(waveId)}/codex-cards`,
    b,
  );

export const createClaudeCard = (waveId: string, b: NewClaudeCardBody) =>
  request<KernelCard>(
    'POST',
    `/api/waves/${encodeURIComponent(waveId)}/claude-cards`,
    b,
  );

// ---------------- fs ----------------

/**
 * Read-only directory listing. Backs the `DirectoryPicker` widget the
 * codex `cwd` field uses. Omit `path` to start at the server's `$HOME`.
 * Response paths are canonicalized — symlinks resolved, `..` collapsed.
 */
export const listDir = (path?: string) => {
  const query = path && path.length > 0
    ? `?path=${encodeURIComponent(path)}`
    : '';
  return request<ListdirResponse>('GET', `/api/fs/listdir${query}`);
};

export const readFile = (path: string) =>
  request<ReadFileResponse>(
    'GET',
    `/api/fs/readfile?path=${encodeURIComponent(path)}`,
  );

export const readFileRaw = (path: string) =>
  `/api/fs/readfile-raw?path=${encodeURIComponent(path)}`;

export const gitStatus = (path: string) =>
  request<GitStatusResponse>(
    'GET',
    `/api/fs/gitstatus?path=${encodeURIComponent(path)}`,
  );

export const gitDiff = (path: string, oldPath?: string) => {
  const qs = new URLSearchParams({ path });
  if (oldPath) qs.set('old_path', oldPath);
  return request<GitDiffResponse>('GET', `/api/fs/gitdiff?${qs.toString()}`);
};

// ---------------- settings ----------------

/**
 * Fetch the app-global settings bag. Always returns 200 with a (possibly
 * empty) `settings` object — never 404, even on a fresh install.
 */
export const getSettings = () =>
  request<SettingsBag>('GET', '/api/settings');

/**
 * Replace the persisted settings. Empty-string or `null` values clear the
 * key on the server (see `routes::settings` for the rationale). The
 * response echoes the resulting bag so the form can re-prime without a
 * second GET.
 */
export const putSettings = (b: SettingsPutBody) =>
  request<SettingsBag>('PUT', '/api/settings', b);

// ---------------- plugin iframe tool-call ----------------

/**
 * Forward a `tools/call` JSON-RPC frame from a plugin iframe to the kernel.
 *
 * The AppBridge instance running in web-calm intercepts the iframe's
 * `app.callServerTool(...)` and hands us `{ name, arguments }`; we POST it
 * to `/api/plugins/:id/tool-call`. The kernel route decides:
 *   - if `name` starts with `neige.`, dispatch into the in-kernel callback
 *     handler (overlays / kv / etc) and never touch the plugin process;
 *   - otherwise reject — per §7.6 row 5, iframes can only call
 *     kernel-namespace tools, never the plugin's own server tools.
 *
 * Throws `CalmApiError` on non-2xx; the AppBridge `oncalltool` wrapper turns
 * those into spec-shaped `CallToolResult { isError: true }` payloads.
 */
export async function toolCallFromIframe(
  pluginId: string,
  body: { name: string; arguments: Record<string, unknown>; call_id?: string },
): Promise<unknown> {
  return request<unknown>(
    'POST',
    `/api/plugins/${encodeURIComponent(pluginId)}/tool-call`,
    body,
  );
}
