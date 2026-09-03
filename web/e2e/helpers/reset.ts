// Per-test reset hook for the `a11y` Playwright project.
//
// The replay binary booted by `_setup/replay-server.setup.ts` serves
// every test in the `a11y` project from a single in-memory kernel. Without
// a between-tests reset, per-test mutations (new tracks, new cards, rename
// edits, view-mode toggles, …) accumulate across the suite and cause
// previously-green specs to flake when their predicates collide with state
// seeded by an earlier planner.
//
// `POST /dev/reset` (declared in `crates/calm-server/src/bin/replay.rs`)
// wipes every row from the in-memory repo and reseeds the original
// fixture's event stream, restoring the "fresh boot" starting state. We
// call it from each `a11y` planner's `beforeEach` so every test sees the
// same starting state regardless of run order.
//
// The endpoint is dev-only — only the `replay --serve` binary mounts it,
// and that binary is itself dev-only (design doc §6.3).

import type { APIRequestContext } from '@playwright/test';

/** Port the replay binary listens on. Duplicated here (rather than imported
 *  from `_setup/replay-server.shared.ts`) because `_setup/` is a
 *  Playwright project file the test runner treats as its own compilation
 *  context — pulling it from a planner creates a circular `testMatch` /
 *  `testIgnore` dependency. Keep in sync with the constant in that file. */
export const REPLAY_PORT = 4141;

/** Hit `POST /dev/reset` on the replay binary. Throws on non-2xx so a
 *  failing reset surfaces immediately in the test that triggered it
 *  rather than producing a confusing assertion failure later. */
export async function resetReplayServer(request: APIRequestContext): Promise<void> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/reset`;
  const response = await request.post(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `resetReplayServer: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
}

/**
 * Issue #175 — mint a user-facing area via the kernel REST API so the
 * keyboard-only tests have a stable anchor in the sidebar after the
 * pre-#175 `Scratch` auto-bootstrap moved into a hidden system area.
 * The replay binary serves the same routes as production, so a direct
 * `POST /api/areas` call here lands a real row backed by an
 * `EventScope::System` `area.updated` event — the live frontend picks
 * it up on the WS feed and the sidebar renders the new area without
 * a reload.
 *
 * The default name `'Atlas'` matches the fixture sweep applied across
 * the unit-test surface (`web/src/pages/Area.test.tsx`,
 * `web/src/app/eventBridge.test.tsx`, `web/src/api/schemas.test.ts`,
 * `web/src/api/queries.test.tsx`) — keeping the e2e suite on the same
 * sentinel makes "where did this area come from?" greppable across the
 * codebase.
 *
 * Returns the area id (UUID, kernel-generated).
 */
export async function createUserArea(
  request: APIRequestContext,
  name = 'Atlas',
  color = '#6a8',
): Promise<{ id: string; name: string }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/areas`;
  const response = await request.post(url, {
    data: { name, color },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `createUserArea: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const area = (await response.json()) as { id: string; name: string };
  return area;
}

/**
 * Issue #175 — mint a track inside an existing area. Counterpart to
 * `createUserArea`; the a11y keyboard suite uses both to set up an
 * `Atlas` area with a `Today` track inside it, replacing the pre-#175
 * auto-bootstrap that put the Today track inside what is now the hidden
 * system area.
 */
export async function createTrackInArea(
  request: APIRequestContext,
  areaId: string,
  title: string,
): Promise<{ id: string; title: string }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/tracks`;
  // #1147 S3 — this helper sends NO `cwd` (and therefore no
  // `attach_folder`). Omitting `cwd` is the *managed workspace* branch:
  // the kernel derives `<workspace-root>/<area>/<track>` and creates the
  // git repository itself, which is what the Today-track bootstrap
  // (`routes/today.rs`) already does on every environment. An explicit
  // `cwd` is the *attached* branch, and since S3 the kernel requires
  // that path to exist and be inside a git work tree — the helper used
  // to invent `/tmp/playwright-area-<id>` and never create it, so the
  // seeded tracks were structurally unusable (any worker on one dies in
  // `git_repo_root_for_track_cwd`) even before the check made it a 400.
  //
  // Consequence for callers: these tracks no longer mint a
  // `area_folders` row. No a11y planner depended on that — the two
  // cascade tests in `a11y-track-area-ops.spec.ts` claim their paths
  // explicitly via `POST /api/areas/:id/folders`, which carries no
  // filesystem contract. It also removes the reason the old signature
  // needed an `attachFolder: false` escape hatch: two tracks in the same
  // area no longer collide on `area_folders.UNIQUE(path)`.
  //
  // `theme` is required end-to-end (#177): the kernel rejects a body
  // without it (422). Pass an inert dark-theme sentinel — the e2e
  // test doesn't probe OSC roundtrips so the concrete RGB doesn't
  // matter, only that the request boundary accepts it.
  const response = await request.post(url, {
    data: {
      area_id: areaId,
      title,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `createTrackInArea: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const track = (await response.json()) as { id: string; title: string };
  return track;
}

/** Seed a track report directly through the replay API origin. */
export async function seedTrackReport(
  request: APIRequestContext,
  trackId: string,
  summary: string,
  body: string,
): Promise<void> {
  const trackUrl = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${encodeURIComponent(trackId)}`;
  const reportUrl = `${trackUrl}/report`;
  const response = await request.post(reportUrl, {
    data: { summary, body, ifDocRev: 0 },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const responseBody = await response.text().catch(() => '<unreadable body>');
    const trackResponse = await request.get(trackUrl);
    const trackBody = await trackResponse.text().catch(() => '<unreadable body>');
    throw new Error(
      `seedTrackReport: POST ${reportUrl} → ${response.status()} ${response.statusText()}: ${responseBody}; GET ${trackUrl} → ${trackResponse.status()} ${trackResponse.statusText()}: ${trackBody}`,
    );
  }
}

/**
 * Seed a renderer-free worker card for specs that need populated card
 * surfaces but do not care about terminal/codex daemon startup. The direct
 * card-create route persists the row and emits `card.added`; the iframe
 * adapter then renders it as an ordinary worker card.
 */
export async function createIframeCard(
  request: APIRequestContext,
  trackId: string,
  url: string,
  sort?: number,
): Promise<{ id: string; kind: string; sort: number }> {
  const endpoint = `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${encodeURIComponent(trackId)}/cards`;
  const response = await request.post(endpoint, {
    data: {
      kind: 'iframe',
      sort,
      payload: { url },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `createIframeCard(${trackId}): POST ${endpoint} -> ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  return (await response.json()) as { id: string; kind: string; sort: number };
}

/**
 * Seed the per-track `view-mode` overlay via the kernel REST API. The
 * header's PR-A binary Cards↔Report switch writes the same row for the
 * `report`/`grid` path, while specs that need list mode can still seed it
 * directly. The body is byte-for-byte the write the removed control used
 * to make: same
 * plugin_id / entity coords / `schemaVersion` as `useOverlayState` +
 * `OVERLAY_VIEW_MODE_SCHEMA_VERSION` in `web/src/pages/Track.tsx`.
 * (`view-mode` is not a kernel-registered overlay kind, so the payload
 * passes `validate_overlay_payload` as an opaque plugin-style kind.)
 * PR-E of #594 restores a writable three-state ViewMode control; specs
 * that want the full click path back can switch then.
 */
export async function seedTrackViewMode(
  request: APIRequestContext,
  trackId: string,
  mode: 'grid' | 'list' | 'report',
): Promise<void> {
  // Relative path on purpose: unlike the other helpers in this file (which
  // are replay-suite-only and pin REPLAY_PORT), this one is also called from
  // the chromium docker project (track-report-view.spec.ts), where 4141 does
  // not exist. Both projects' request contexts resolve /api against the
  // right stack — the a11y vite server proxies /api to the replay binary
  // via VITE_API_PROXY_TARGET.
  const url = `/api/overlays`;
  const response = await request.post(url, {
    data: {
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: trackId,
      kind: 'view-mode',
      payload: { schemaVersion: 1, mode },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `seedTrackViewMode(${trackId}, ${mode}): POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
}
