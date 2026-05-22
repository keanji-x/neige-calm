// REST client for calm-server. One function per kernel route; thin wrapper
// over `fetch` that throws on non-2xx with the server's `{error, code}` body.

import { fireUnauthorized } from './onUnauthorized';
import type {
  CardPatchBody,
  CovePatchBody,
  KernelCard,
  KernelCove,
  KernelOverlay,
  KernelTerminal,
  KernelWave,
  KernelWaveDetail,
  ListdirResponse,
  NewCardBody,
  NewCodexCardBody,
  NewCoveBody,
  NewOverlayBody,
  NewTerminalCardBody,
  NewWaveBody,
  SettingsBag,
  SettingsPutBody,
  WavePatchBody,
} from './wire';

export class CalmApiError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, msg: string) {
    super(msg);
    this.status = status;
    this.code = code;
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
    try {
      const j = await res.json();
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
    throw new CalmApiError(res.status, code, msg);
  }
  // 200 / 201 with body
  return (await res.json()) as T;
}

// ---------------- coves ----------------

export const listCoves = () =>
  request<KernelCove[]>('GET', '/api/coves');
export const createCove = (b: NewCoveBody) =>
  request<KernelCove>('POST', '/api/coves', b);
export const updateCove = (id: string, b: CovePatchBody) =>
  request<KernelCove>('PATCH', `/api/coves/${encodeURIComponent(id)}`, b);
export const deleteCove = (id: string) =>
  request<void>('DELETE', `/api/coves/${encodeURIComponent(id)}`);
export const wavesInCove = (coveId: string) =>
  request<KernelWave[]>('GET', `/api/coves/${encodeURIComponent(coveId)}/waves`);

// ---------------- waves ----------------

export const createWave = (b: NewWaveBody) =>
  request<KernelWave>('POST', '/api/waves', b);
export const getWaveDetail = (id: string) =>
  request<KernelWaveDetail>('GET', `/api/waves/${encodeURIComponent(id)}`);
export const updateWave = (id: string, b: WavePatchBody) =>
  request<KernelWave>('PATCH', `/api/waves/${encodeURIComponent(id)}`, b);
export const deleteWave = (id: string) =>
  request<void>('DELETE', `/api/waves/${encodeURIComponent(id)}`);

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
 * linked terminal row, AND spawns the `calm-session-daemon`. Server emits a
 * single `card.added` event carrying the final payload — no intermediate
 * `payload=null` flash for EventBridge to swallow. See `routes::terminal_cards`
 * and issue #13.
 *
 * 500 response means the daemon spawn failed; the persisted rows stay (the
 * orphan-terminal sweeper reaps them within ~60s).
 */
export const createTerminalCard = (waveId: string, b: NewTerminalCardBody = {}) =>
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
export const createCodexCard = (waveId: string, b: NewCodexCardBody = {}) =>
  request<KernelCard>(
    'POST',
    `/api/waves/${encodeURIComponent(waveId)}/codex-cards`,
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
