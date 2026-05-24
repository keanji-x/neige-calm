// E2E: `GET /api/coves/resolve?path=<cwd>` cwd → cove resolution
// across multiple coves with folder claims (#269 P3).
//
// Unit coverage already exists for `normalize_path` and
// `is_descendant_of` in `crates/calm-server/src/routes/cove_folders.rs`;
// integration coverage for the CRUD + overlap-409 invariants lives in
// `crates/calm-server/tests/cove_folders.rs`. This spec is the missing
// HTTP-level cwd-resolution test exercised through the web's wire
// path. It pins the end-to-end contracts a `make dev` user actually
// hits when their browser asks "which cove owns this cwd?":
//
//   * with two distinct coves owning disjoint paths, a descendant
//     query resolves to the right cove (not the other);
//   * within a single cove, sibling rows that share a parent prefix
//     (`/p/alpha` + `/p/alpha-extra`) resolve to their own claim,
//     never to the other;
//   * an exact-path query matches the row for that exact claim;
//   * a query with a trailing slash normalizes to the same answer as
//     without (server-side `normalize_path` is on the wire path);
//   * a query outside every claim returns JSON `null` (200, not 404
//     or 500);
//   * a sibling-prefix that *looks* like a string prefix but is not a
//     filesystem ancestor (e.g. `/work/repository` vs `/work/repo`)
//     is NOT a match — the `is_descendant_of` guard requires a `/`
//     boundary.
//
// Note on scope: the resolve handler's `max_by_key(|f| f.path.len())`
// longest-prefix tiebreak is NOT covered here. That branch is dead
// from the HTTP surface — the create endpoint rejects
// ancestor/descendant overlap with 409 (see
// `cove_folders.rs:135-156`), so the filter can never return more
// than one row from any HTTP-constructible state. The unit test
// `resolve_picks_longest_prefix` in
// `crates/calm-server/tests/cove_folders.rs` exercises the branch by
// seeding overlapping rows through the raw repo, which is the right
// level for that kind of "what if the DB is corrupted" probe.
//
// Runs in the hermetic `a11y` Playwright project so the REST surface
// is exercised against the in-memory replay binary (no `make dev`
// dependency; no cross-spec state bleed thanks to
// `resetReplayServer`).
//
// Note: there is no cove-folder management UI in the web app as of
// #269 P3 (Settings page covers proxy config only; Cove/Wave pages
// don't expose folder mgmt). The second P3 checkbox in the issue
// ("cove settings page (如果有) 管理 cove 的 folders 列表") is
// therefore intentionally NOT covered by a UI smoke here — see the PR
// body for the explicit skip note.

import { test, expect, type APIRequestContext } from '@playwright/test';
import { REPLAY_PORT, createUserCove, resetReplayServer } from './helpers/reset';

const API = `http://127.0.0.1:${REPLAY_PORT}`;

type ResolveBody = { cove_id: string; folder_id: number; folder_path: string };

/** Claim `path` for `coveId` via `POST /api/coves/:id/folders`. Asserts
 *  201 so a server-side conflict (overlap with an existing claim, bad
 *  path shape, etc.) surfaces in the spec that triggered it instead
 *  of as a confusing later assertion failure. */
async function claimFolder(
  request: APIRequestContext,
  coveId: string,
  path: string,
): Promise<void> {
  const res = await request.post(`${API}/api/coves/${coveId}/folders`, {
    data: { path },
    headers: { 'content-type': 'application/json' },
  });
  if (res.status() !== 201) {
    const body = await res.text().catch(() => '<unreadable body>');
    throw new Error(
      `claimFolder(${coveId}, ${path}) → ${res.status()} ${res.statusText()}: ${body}`,
    );
  }
}

/** Hit `GET /api/coves/resolve?path=<path>` and return the parsed body
 *  (or `null` for a miss). Asserts 200 so server-side errors surface
 *  loudly. */
async function resolvePath(
  request: APIRequestContext,
  path: string,
): Promise<ResolveBody | null> {
  const res = await request.get(`${API}/api/coves/resolve`, {
    params: { path },
  });
  expect(
    res.status(),
    `GET /api/coves/resolve?path=${path} expected 200`,
  ).toBe(200);
  return (await res.json()) as ResolveBody | null;
}

test.beforeEach(async ({ request }) => {
  // Hermetic per-test state. Resolve scans every row in `cove_folders`
  // and returns the (unique, post-409-invariant) covering match;
  // without a reset, claims from an earlier test would shadow the
  // fixtures this test sets up.
  await resetReplayServer(request);
});

test('multi-cove disjoint claims resolve to the correct cove', async ({ request }) => {
  // Two coves with disjoint folder claims. A descendant query of each
  // claim MUST resolve to its owning cove (no cross-talk), and a
  // query outside both MUST return null. This is the everyday
  // multi-cove cwd contract — without it, `make dev` users would land
  // waves in whichever cove the surrounding page hinted at instead of
  // the one that actually owns the cwd.
  //
  // Paths are namespaced per-run so a concurrent / repeated run on a
  // shared server can't trip cove_folders.UNIQUE(path). In the a11y
  // project this is hermetic via resetReplayServer; the convention
  // makes the spec safe to re-read against a non-hermetic server.
  const ts = Date.now();
  const pathA = `/work-${ts}-alpha`;
  const pathB = `/work-${ts}-bravo`;

  const coveA = await createUserCove(request, `cove-A-${ts}`, '#5a9');
  const coveB = await createUserCove(request, `cove-B-${ts}`, '#a75');

  await claimFolder(request, coveA.id, pathA);
  await claimFolder(request, coveB.id, pathB);

  // Descendant of cove A's claim → cove A.
  const hitA = await resolvePath(request, `${pathA}/repo/file.rs`);
  expect(hitA).not.toBeNull();
  expect(hitA!.cove_id).toBe(coveA.id);
  expect(hitA!.folder_path).toBe(pathA);

  // Descendant of cove B's claim → cove B.
  const hitB = await resolvePath(request, `${pathB}/repo/file.rs`);
  expect(hitB).not.toBeNull();
  expect(hitB!.cove_id).toBe(coveB.id);
  expect(hitB!.folder_path).toBe(pathB);

  // Outside every claim → null (200, not 404).
  const miss = await resolvePath(request, `/elsewhere-${ts}/file.rs`);
  expect(miss).toBeNull();
});

test('similar-prefix sibling claims resolve to the correct row', async ({
  request,
}) => {
  // One cove owning two sibling folders that share a parent prefix
  // (`/p/alpha` and `/p/alpha-extra` — both have `/p/alpha` as a raw
  // string prefix). A descendant query under each MUST resolve to
  // its own claim, never to the other, because `is_descendant_of`'s
  // `/`-boundary guard rules out `/p/alpha-extra/...` as a descendant
  // of `/p/alpha` (the substring match would be a bug — see the
  // dedicated sibling-prefix test below for the unit-level guard
  // expressed as a resolve miss).
  //
  // This complements the "multi-cove disjoint" case above by pinning
  // the same contract within a *single* cove with multiple rows in
  // `cove_folders` — the resolve scan is across ALL rows so the
  // single-cove path goes through the same filter code.
  //
  // Note on the absent nested case: we can't seed two nested claims
  // (`/p` + `/p/alpha`) via the HTTP surface because the create
  // endpoint rejects ancestor/descendant overlap as a 409 conflict
  // (see `crates/calm-server/tests/cove_folders.rs` cases 2-4 for
  // the invariant + cross_cove_overlap_409_descendant for the
  // multi-cove dimension).
  const ts = Date.now();
  const shortClaim = `/proj-${ts}/alpha`;
  const longClaim = `/proj-${ts}/alpha-extra`;

  const cove = await createUserCove(request, `cove-prefix-${ts}`, '#5a9');
  await claimFolder(request, cove.id, shortClaim);
  await claimFolder(request, cove.id, longClaim);

  // Descendant of `longClaim` resolves to `longClaim` — NOT to
  // `shortClaim`, which doesn't cover it because of the `/` boundary
  // guard.
  const hitLong = await resolvePath(request, `${longClaim}/inner/file.rs`);
  expect(hitLong).not.toBeNull();
  expect(hitLong!.folder_path).toBe(longClaim);

  // Descendant of `shortClaim` (under `/alpha/` not `/alpha-extra/`)
  // resolves to `shortClaim`.
  const hitShort = await resolvePath(request, `${shortClaim}/inner/file.rs`);
  expect(hitShort).not.toBeNull();
  expect(hitShort!.folder_path).toBe(shortClaim);
});

test('exact-path queries match the claim with equal path', async ({ request }) => {
  const ts = Date.now();
  const pathA = `/work-${ts}-alpha`;
  const pathB = `/work-${ts}-bravo`;

  const coveA = await createUserCove(request, `cove-A-${ts}`, '#5a9');
  const coveB = await createUserCove(request, `cove-B-${ts}`, '#a75');

  await claimFolder(request, coveA.id, pathA);
  await claimFolder(request, coveB.id, pathB);

  // Exact match on cove A's claim → A. `is_descendant_of(p, p)` is
  // true for the equal case, so the resolve handler treats an
  // exact-path query the same as a descendant-of-itself query.
  const exactA = await resolvePath(request, pathA);
  expect(exactA).not.toBeNull();
  expect(exactA!.cove_id).toBe(coveA.id);
  expect(exactA!.folder_path).toBe(pathA);

  // Exact match on cove B's claim → B.
  const exactB = await resolvePath(request, pathB);
  expect(exactB).not.toBeNull();
  expect(exactB!.cove_id).toBe(coveB.id);
  expect(exactB!.folder_path).toBe(pathB);
});

test('trailing-slash queries normalize to the same answer', async ({ request }) => {
  const ts = Date.now();
  const claim = `/work-${ts}-alpha`;

  const cove = await createUserCove(request, `cove-only-${ts}`, '#5a9');
  await claimFolder(request, cove.id, claim);

  // Trailing slash is trimmed by `normalize_path` on the server before
  // matching; the resolve answer MUST be identical to the slashless
  // query for both an exact and a descendant-style probe.
  const exactWithSlash = await resolvePath(request, `${claim}/`);
  const exactWithoutSlash = await resolvePath(request, claim);
  expect(exactWithSlash).toEqual(exactWithoutSlash);
  expect(exactWithSlash!.cove_id).toBe(cove.id);

  const descWithSlash = await resolvePath(request, `${claim}/file.rs/`);
  const descWithoutSlash = await resolvePath(request, `${claim}/file.rs`);
  expect(descWithSlash).toEqual(descWithoutSlash);
  expect(descWithSlash!.cove_id).toBe(cove.id);
});

test('sibling-prefix path is NOT a match (guards against naive string prefix)', async ({
  request,
}) => {
  // Regression guard for `is_descendant_of` correctly requiring a `/`
  // boundary between the claim and the rest of the query. Without
  // that guard, claiming `/work-<ts>/repo` would (incorrectly) cover
  // `/work-<ts>/repository` because the latter starts with the
  // former as a substring. The handler's filter must reject this
  // case.
  const ts = Date.now();
  const claim = `/work-${ts}/repo`;
  const siblingPrefix = `/work-${ts}/repository`;

  const cove = await createUserCove(request, `cove-only-${ts}`, '#5a9');
  await claimFolder(request, cove.id, claim);

  // Exact match on the claim itself still works.
  const exact = await resolvePath(request, claim);
  expect(exact).not.toBeNull();
  expect(exact!.folder_path).toBe(claim);

  // Sibling-prefix string match must MISS — no claim covers
  // `/work-<ts>/repository`.
  const miss = await resolvePath(request, siblingPrefix);
  expect(miss).toBeNull();

  const missDeeper = await resolvePath(request, `${siblingPrefix}/file.rs`);
  expect(missDeeper).toBeNull();
});

// Backend invariant: the create-folder endpoint rejects
// ancestor/descendant overlap with 409 + `FolderConflict`. That
// invariant is what keeps the resolve handler's filter set to at most
// one row per query path. It is exercised at integration-test speed
// in `crates/calm-server/tests/cove_folders.rs` (cases (3) ancestor,
// (4) descendant, (2) equal, plus the cross-cove case
// `cross_cove_overlap_409_descendant`). It does not need a Playwright
// spec — the wire path here adds no signal beyond the Rust integration
// test and pays the browser tax for nothing.

// ---------------------------------------------------------------------------
// Edge cases called out in the #274 review, added as a #269 follow-up.
// ---------------------------------------------------------------------------

test('root `/` claim covers every absolute path on that cove', async ({ request }) => {
  // Regression guard for `is_descendant_of`'s root special case at
  // `crates/calm-server/src/routes/cove_folders.rs:74-77`. With
  // `parent == "/"`, the function returns `true` for any candidate
  // that itself starts with `/`. Without the early-return branch,
  // the fallback would build `format!("{parent}/")` = `"//"` and
  // miss every real cwd query.
  //
  // We claim `/` for cove A *without* using `createWaveInCove`
  // (which auto-attaches `/tmp/playwright-cove-<id>` and would
  // collide with the root claim under the create endpoint's
  // ancestor/descendant overlap 409 — see the integration test
  // `crates/calm-server/tests/cove_folders.rs`). The `beforeEach`
  // reset drops every cove (and via `ON DELETE CASCADE` on
  // `cove_folders.cove_id`, every claim) so the root row this test
  // creates is the only one in the table when the resolve runs.
  const ts = Date.now();
  const cove = await createUserCove(request, `cove-root-${ts}`, '#5a9');
  await claimFolder(request, cove.id, '/');

  // Deep descendant resolves to cove A.
  const hitDeep = await resolvePath(request, `/work-${ts}/repo/file.rs`);
  expect(hitDeep).not.toBeNull();
  expect(hitDeep!.cove_id).toBe(cove.id);
  expect(hitDeep!.folder_path).toBe('/');

  // Shallow descendant resolves to cove A.
  const hitShallow = await resolvePath(request, `/anything-${ts}`);
  expect(hitShallow).not.toBeNull();
  expect(hitShallow!.cove_id).toBe(cove.id);
  expect(hitShallow!.folder_path).toBe('/');

  // The root itself resolves to cove A (`is_descendant_of("/", "/")`
  // is true via the equality branch above the root-special-case).
  const hitRoot = await resolvePath(request, '/');
  expect(hitRoot).not.toBeNull();
  expect(hitRoot!.cove_id).toBe(cove.id);
  expect(hitRoot!.folder_path).toBe('/');
});

test('empty `path` query rejects with 400', async ({ request }) => {
  // `?path=` deserializes as `q.path == ""`. The handler's first
  // guard is `!q.path.starts_with('/')` — empty string doesn't
  // start with `/`, so the rejection lands as a 400 BadRequest
  // (`CalmError::BadRequest` → `StatusCode::BAD_REQUEST`).
  //
  // The wire body is the `{error, code}` shape from
  // `error.rs::ErrorBody`; pin `code == "bad_request"` so a future
  // refactor that swaps the error variant (e.g. to a new
  // `EmptyPath` enum) is forced to update this assertion
  // deliberately rather than silently slipping through.
  const res = await request.get(`${API}/api/coves/resolve`, {
    params: { path: '' },
  });
  expect(res.status(), 'empty path must reject with 400').toBe(400);
  const body = (await res.json()) as { code?: string; error?: string };
  expect(body.code).toBe('bad_request');
  expect(body.error ?? '').toMatch(/absolute/i);
});

test('non-absolute `path` query rejects with 400', async ({ request }) => {
  // The handler explicitly rejects any path that doesn't start with
  // `/` — `relative/path` is the canonical regression case. Same
  // `CalmError::BadRequest` → 400 mapping as the empty-path case
  // above, but the error message references the offending input so
  // we additionally pin that the bad value lands in the body
  // (operators reading logs need it).
  const res = await request.get(`${API}/api/coves/resolve`, {
    params: { path: 'relative/path' },
  });
  expect(res.status(), 'non-absolute path must reject with 400').toBe(400);
  const body = (await res.json()) as { code?: string; error?: string };
  expect(body.code).toBe('bad_request');
  expect(body.error ?? '').toMatch(/absolute/i);
  expect(body.error ?? '').toMatch(/relative\/path/);
});
