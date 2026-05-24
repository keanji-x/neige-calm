// TanStack Router setup — code-based (not file-based).
//
// Routes:
//   /                  → TodayPage
//   /cove/$coveId      → CovePage
//   /wave/$waveId      → WavePage
//
// The root route renders <CalmApp /> as a layout shell; CalmApp owns
// Sidebar + TitleBar and emits an <Outlet /> for the matched route.
//
// Each route component below sources its data via TanStack Query hooks
// from `api/queries.ts`. The kernel data is no longer threaded through
// a shared context — Query handles caching, deduplication, and refetch.
// WS events translate to query invalidations in `app/eventBridge.tsx`.
//
// Each route declares a `loader` that primes the relevant TanStack Query
// cache entries via `queryClient.ensureQueryData(...)` before the route
// component mounts. The matching `useQuery` hook inside the component
// then reads the already-cached data instantly — no per-route spinner
// flash on navigation. The loader uses the same `{ queryKey, queryFn }`
// factories exported from `api/queries.ts`, so cache shape stays in lock-
// step with the hook call sites.

import { lazy, useMemo } from 'react';
import { useState } from '../shared/state';
import {
  createRootRoute,
  createRoute,
  createRouter,
  useParams,
} from '@tanstack/react-router';
import { CalmApp } from '../CalmApp';
import { MissingShell } from './shell';
import { useGo } from './navigation';
import { useTodayTerminal } from '../hooks/useTodayTerminal';
import {
  covesQueryOptions,
  settingsQueryOptions,
  useCovesQuery,
  useDeleteCardMutation,
  useDeleteCoveMutation,
  useDeleteWaveMutation,
  useOverlaysByKindQuery,
  useUpdateCoveMutation,
  useUpdateWaveMutation,
  useWaveDetailQuery,
  useWavesByCoveQuery,
  useWavesRangeQuery,
  waveDetailQueryOptions,
  wavesByCoveQueryOptions,
  wavesRangeQueryOptions,
} from '../api/queries';
import { startOfWeek } from '../pages/Calendar';
import type { CalendarWave } from '../pages/Calendar';
import { adaptCard, adaptCove, adaptWave } from '../api/adapt';
import * as api from '../api/calm';
import { DARK_THEME_RGB, LIGHT_THEME_RGB } from '../api/themeRgb';
import { useQueryClient, useQueries } from '@tanstack/react-query';
import { queryKeys } from '../api/queries';
import { queryClient } from './providers';
import { dlog } from '../util/debug';
import type { Cove, Wave, WaveCardSlot } from '../types';
import type { AddPanelKind } from '../shared/components/AddPanel';

// Per-route page components are loaded on demand so the entry chunk only
// carries the shell + routing wiring; each page's code ships as its own
// chunk fetched when the user navigates. The route `loader` runs in
// parallel with the JS download, so query data is primed by the time the
// lazy component resolves — no cascading waterfall.
//
// CalmApp wraps <Outlet /> in <Suspense>, providing a single fallback for
// every lazy route component below.
const TodayPage = lazy(() =>
  import('../pages/Today').then((m) => ({ default: m.TodayPage })),
);
const CovePage = lazy(() =>
  import('../pages/Cove').then((m) => ({ default: m.CovePage })),
);
const WavePage = lazy(() =>
  import('../pages/Wave').then((m) => ({ default: m.WavePage })),
);
const SettingsPage = lazy(() =>
  import('../pages/Settings').then((m) => ({ default: m.SettingsPage })),
);
const CalendarPage = lazy(() =>
  import('../pages/Calendar').then((m) => ({ default: m.CalendarPage })),
);

// ---------- Route tree ----------

const rootRoute = createRootRoute({
  component: CalmApp,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  // Today fans out to per-cove wave lists on the page itself; we
  // conservatively prefetch only the coves list here. The cove → waves
  // fan-out stays lazy (the page uses `useQueries`) so a slow cove
  // doesn't block the calendar.
  loader: () => queryClient.ensureQueryData(covesQueryOptions()),
  component: IndexComponent,
});

const coveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/cove/$coveId',
  loader: ({ params }) =>
    queryClient.ensureQueryData(wavesByCoveQueryOptions(params.coveId)),
  component: CoveComponent,
});

const waveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/wave/$waveId',
  loader: ({ params }) =>
    queryClient.ensureQueryData(waveDetailQueryOptions(params.waveId)),
  component: WaveComponent,
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  // Prime the settings cache so the form fills in without a spinner flash
  // on the first visit. Cheap (one tiny GET) and falls back to a loading
  // state inside the page itself on a slow link.
  loader: () => queryClient.ensureQueryData(settingsQueryOptions()),
  component: SettingsComponent,
});

/**
 * Issue #250 PR 5 — `/calendar` route. Prime BOTH the coves list (so
 * bars can colour-code immediately on first paint) AND the current
 * week's wave window. The loader runs in parallel with the lazy
 * `CalendarPage` chunk download — by the time the component mounts
 * the data is already in cache and `useWavesRangeQuery` reads it
 * synchronously.
 */
const calendarRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/calendar',
  loader: () => {
    const { since, until } = calendarWindowMs(Date.now());
    return Promise.all([
      queryClient.ensureQueryData(covesQueryOptions()),
      queryClient.ensureQueryData(wavesRangeQueryOptions(since, until)),
    ]);
  },
  component: CalendarComponent,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  calendarRoute,
  coveRoute,
  waveRoute,
  settingsRoute,
]);

// `basepath` mirrors Vite's `base: '/calm/'` (see vite.config.ts) so URLs
// in the browser actually read `/calm/cove/$id` rather than `/cove/$id`.
// Router internals (route definitions above, useRouterState's pathname)
// still see paths relative to the basepath — only the browser URL and
// generated <a href> include the prefix.
export const router = createRouter({
  routeTree,
  basepath: '/calm',
  defaultPreload: false,
});

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}

// ---------- Route page components ----------

function IndexComponent() {
  const go = useGo();
  const covesQ = useCovesQuery();
  // Belt-and-suspenders for issue #175 — see CalmApp.tsx for the same
  // filter. The server already hides `kind='system'` from
  // `GET /api/coves` by default, but the second layer of defence keeps
  // the system cove out of Today's calendar/clock fan-out as well.
  const kernelCoves = (covesQ.data ?? []).filter((c) => c.kind === 'user');

  // Today's calendar + clock want a flat wave list across all coves.
  // One query per cove keeps cache granularity sensible (a wave moving
  // between coves invalidates only the two affected lists).
  const waveQueries = useQueries({
    queries: kernelCoves.map((c) => ({
      queryKey: queryKeys.wavesInCove(c.id),
      queryFn: () => api.wavesInCove(c.id),
    })),
  });

  // Workspace-wide wave overlays — fed into adaptWave so the Sidebar's
  // status indicators ("waiting on you" / "running") are accurate for
  // every wave, not just whichever wave the user has opened. eventBridge
  // invalidates this snapshot on overlay.set/.deleted and on wave/cove
  // deletes (where the kernel may not cascade individual events).
  const waveOverlaysQ = useOverlaysByKindQuery('wave');
  const overlaysByWaveId = new Map<string, typeof waveOverlaysQ.data>();
  for (const o of waveOverlaysQ.data ?? []) {
    if (o.entity_kind !== 'wave') continue;
    const cur = overlaysByWaveId.get(o.entity_id);
    if (cur) cur.push(o);
    else overlaysByWaveId.set(o.entity_id, [o]);
  }

  const coves: Cove[] = kernelCoves.map(adaptCove);
  const waves: Wave[] = [];
  for (const q of waveQueries) {
    if (!q.data) continue;
    for (const w of q.data) {
      waves.push(adaptWave(w, overlaysByWaveId.get(w.id) ?? []));
    }
  }

  const todayTerm = useTodayTerminal();

  return (
    <TodayPage
      waves={waves}
      coves={coves}
      onGo={go}
      todayTerminalId={todayTerm.today?.terminalId ?? null}
      todayError={todayTerm.error}
      onResetTodayTerminal={todayTerm.reset}
    />
  );
}

function CoveComponent() {
  const go = useGo();
  const { coveId } = useParams({ from: coveRoute.id });
  const covesQ = useCovesQuery();
  const wavesQ = useWavesByCoveQuery(coveId);
  const updateCove = useUpdateCoveMutation();
  const deleteCove = useDeleteCoveMutation();
  const deleteWave = useDeleteWaveMutation();

  const kernelCove = covesQ.data?.find((c) => c.id === coveId);
  if (!kernelCove) {
    // While the coves list is loading, we don't know if the cove exists.
    // Show the calm "Connecting…" shell rather than flashing a missing
    // state. CalmApp already renders LoadingShell for the initial fetch,
    // but a hard-refresh on /cove/:id can land here before cache primes.
    if (covesQ.isLoading) return null;
    return <MissingShell label="Cove" onGo={go} />;
  }
  const cove = adaptCove(kernelCove);
  const waves: Wave[] = (wavesQ.data ?? []).map((w) => adaptWave(w, []));

  return (
    <CovePage
      cove={cove}
      waves={waves}
      onGo={go}
      onWaveCreated={(wave) => {
        // Issue #250 PR 3 — the NewTaskForm inside CovePage owns the
        // wave-create POST end-to-end (cwd + cove auto-inference +
        // theme stamping + folder-conflict surfacing). All this
        // callback needs to do is navigate. The cwd-empty stopgap
        // from PR 2 is gone — the form refuses to submit without a
        // valid absolute path.
        go({ name: 'wave', id: wave.id });
      }}
      onRenameCove={async (cId, name) => {
        try {
          await updateCove.mutateAsync({ id: cId, body: { name } });
        } catch (err) {
          console.warn('[Calm] cove rename failed:', err);
        }
      }}
      onDeleteCove={async (cId) => {
        try {
          await deleteCove.mutateAsync(cId);
          go({ name: 'today' });
        } catch (err) {
          console.warn('[Calm] cove delete failed:', err);
        }
      }}
      onDeleteWave={async (waveId) => {
        try {
          await deleteWave.mutateAsync({ id: waveId, coveId: cove.id });
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
    />
  );
}

function SettingsComponent() {
  const go = useGo();
  return <SettingsPage onGo={go} />;
}

/**
 * Issue #250 PR 5 — convert "any timestamp" into a `[since, until]` ms
 * pair bracketing the local-time week that owns it. Mon 00:00 →
 * Sun 23:59:59.999. Exported sibling of `startOfWeek` so the route
 * loader and CalendarComponent share one formula (and so the cache
 * key shape is identical between prefetch and live read).
 */
function calendarWindowMs(anchorMs: number): { since: number; until: number } {
  const start = startOfWeek(new Date(anchorMs));
  const end = new Date(start);
  end.setDate(end.getDate() + 6);
  end.setHours(23, 59, 59, 999);
  return { since: start.getTime(), until: end.getTime() };
}

function CalendarComponent() {
  const go = useGo();
  // Anchor lives here (not inside CalendarPage) so the React Query key
  // and the page state move in lock-step. CalendarPage takes
  // `weekAnchor` as a controlled prop and reports back via
  // `onWeekChange` — that way the query refetch and the visible week
  // header can never disagree.
  const [anchor, setAnchor] = useState<number>(() => Date.now());
  const window = useMemo(() => calendarWindowMs(anchor), [anchor]);
  const wavesQ = useWavesRangeQuery(window.since, window.until);
  const covesQ = useCovesQuery();

  // System cove waves still surface through the API (per PR 2's
  // "system cove exempt from cwd-claim" rule). Drop them for the
  // user-facing calendar — they're scaffolding noise, same reason
  // the sidebar filters them at CalmApp.
  const userCoves = useMemo(
    () => (covesQ.data ?? []).filter((c) => c.kind === 'user'),
    [covesQ.data],
  );
  const userCoveIds = useMemo(() => new Set(userCoves.map((c) => c.id)), [userCoves]);

  const adaptedCoves = useMemo(
    () => userCoves.map((c) => ({ id: c.id, name: c.name, subtitle: '', color: c.color })),
    [userCoves],
  );

  const calendarWaves: CalendarWave[] = useMemo(() => {
    const out: CalendarWave[] = [];
    for (const w of wavesQ.data ?? []) {
      if (!userCoveIds.has(w.cove_id)) continue;
      out.push({
        id: w.id,
        title: w.title,
        coveId: w.cove_id,
        lifecycle: w.lifecycle ?? 'draft',
        createdAt: w.created_at,
        terminalAt: w.terminal_at ?? null,
        cwd: w.cwd ?? '',
      });
    }
    return out;
  }, [wavesQ.data, userCoveIds]);

  return (
    <CalendarPage
      waves={calendarWaves}
      coves={adaptedCoves}
      weekAnchor={anchor}
      onGo={go}
      onWeekChange={setAnchor}
    />
  );
}

function WaveComponent() {
  const go = useGo();
  const { waveId } = useParams({ from: waveRoute.id });
  const detailQ = useWaveDetailQuery(waveId);
  const covesQ = useCovesQuery();
  const qc = useQueryClient();
  const updateWave = useUpdateWaveMutation();
  const deleteWave = useDeleteWaveMutation();
  const deleteCard = useDeleteCardMutation();
  dlog('WaveComponent', 'render', {
    waveId,
    detailLoaded: !!detailQ.data,
    cardsCount: detailQ.data?.cards.length,
    detailFetchStatus: detailQ.fetchStatus,
    detailStatus: detailQ.status,
  });

  // Wave detail is the source of truth for "does this wave exist?".
  if (!detailQ.data) {
    if (detailQ.isLoading) return null;
    return <MissingShell label="Wave" onGo={go} />;
  }
  const detail = detailQ.data;
  const kernelCove = covesQ.data?.find((c) => c.id === detail.wave.cove_id);
  if (!kernelCove) {
    if (covesQ.isLoading) return null;
    return <MissingShell label="Cove" onGo={go} />;
  }
  const cove = adaptCove(kernelCove);
  const uiWave = adaptWave(detail.wave, detail.overlays);
  uiWave.cards = detail.cards.map((k): WaveCardSlot => {
    // Issue #229 PR A — propagate the kernel's `deletable` bit so
    // `WaveGrid` can suppress the close X on kernel-owned cards.
    // OpenAPI emits `deletable: boolean`, so the field is always set
    // on fresh wire payloads; legacy event-log replays may omit it,
    // and the slot's `deletable?` default + WaveGrid's
    // `card.deletable !== false` check both treat undefined as
    // "user-deletable" (matches the DB DEFAULT of 1).
    const adapted = adaptCard(k);
    if (adapted)
      return {
        kind: 'card',
        card: adapted,
        sort: k.sort,
        deletable: k.deletable,
      };
    return {
      kind: 'unknown',
      id: k.id,
      kernelKind: k.kind,
      sort: k.sort,
      deletable: k.deletable,
    };
  });

  return (
    <WavePage
      wave={uiWave}
      cove={cove}
      onGo={go}
      onAddCard={async (wId, type) => {
        // #177 — click-time host-theme read; see the matching
        // comment on `onCreateCardWithBody` below. Same rationale
        // (no `useTheme()` here → no theme-driven wave-subtree
        // re-render → XtermView stays mounted across the toggle).
        const theme: 'light' | 'dark' =
          typeof document !== 'undefined' &&
          document.documentElement.dataset.theme === 'light'
            ? 'light'
            : 'dark';
        await addCardOfKind(qc, wId, type, theme);
      }}
      onCreateCardWithBody={async (wId, type, values) => {
        // #177 — read the resolved theme at click-time from
        // `<html data-theme>` rather than subscribing to
        // ThemeContext via `useTheme()` in this component.
        // Subscribing would re-render the wave subtree on every
        // theme toggle and trip TanStack Router's `<Match>`
        // Suspense boundary, remounting any live XtermView and
        // wiping its `pendingThemeRef`. `ThemeProvider` mirrors
        // `resolved` into `<html data-theme>` synchronously
        // (see `app/theme.tsx`), so this read is always current.
        const theme: 'light' | 'dark' =
          typeof document !== 'undefined' &&
          document.documentElement.dataset.theme === 'light'
            ? 'light'
            : 'dark';
        await addCardWithValues(qc, wId, type, values, theme);
      }}
      onRemoveCard={async (_wId, idx) => {
        const target = detail.cards[idx];
        if (!target) return;
        try {
          await deleteCard.mutateAsync({ id: target.id, waveId: detail.wave.id });
        } catch (err) {
          console.warn('[Calm] card delete failed:', err);
        }
      }}
      onRenameWave={async (wId, title) => {
        try {
          await updateWave.mutateAsync({ id: wId, body: { title } });
        } catch (err) {
          console.warn('[Calm] wave rename failed:', err);
        }
      }}
      onDeleteWave={async (wId) => {
        try {
          await deleteWave.mutateAsync({ id: wId, coveId: cove.id });
          go({ name: 'cove', coveId: cove.id });
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
    />
  );
}

/**
 * Card create routed by kind. Terminal cards need the two-step "card
 * row + Terminal row + payload patch" dance the kernel expects.
 *
 * Lives here (not in queries.ts) because it composes three mutations
 * in sequence; wrapping that in `useMutation` would obscure the
 * sequencing for not much gain. We call the api client directly and
 * trigger the wave-detail invalidation manually.
 *
 * Non-terminal kinds (doc/git/diff/plan) were removed in Wave 4; new
 * card kinds will arrive through the plugin host (M3) as `ui://` cards,
 * not as additional built-ins, so this function intentionally only
 * handles `'terminal'`.
 */
/**
 * Schema-driven card create. The Wave page hands us the kind + the
 * SchemaForm values; we look up the right kernel sequence per kind.
 *
 * Today only `codex` flows through here (multi-field input). Terminal
 * stays on `addCardOfKind` (no schema → default args). Other kinds
 * (`plugin:*` / `ui://*`) come through their own create path via the
 * plugin host; they're not menu-driven from the AddPanel.
 */
async function addCardWithValues(
  qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  type: AddPanelKind,
  values: Record<string, string>,
  theme: 'light' | 'dark',
): Promise<void> {
  if (type !== 'codex') {
    // Falls through to the default "no-config" pathway. The AddPanel
    // shouldn't surface a schema form for kinds without `createSchema`,
    // so this is defensive only.
    return addCardOfKind(qc, waveId, type, theme);
  }
  try {
    dlog('addCardWithValues', 'codex create START', { waveId, values, theme });
    // Atomic codex-card create (#117). One round-trip writes the card row,
    // the linked terminal row, payload (with `terminal_id` + optional
    // `cwd`), AND spawns the codex daemon. Server emits a single
    // `card.added` event carrying the final payload — no intermediate
    // empty-payload flash for the renderer's "Codex is starting…"
    // placeholder to react to.
    //
    // #177 — stamp the host browser's current theme RGB onto the body.
    // The kernel forwards this to the codex daemon's argv so its
    // `TerminalModel` answers codex's OSC 10/11 startup probe with
    // matching colors; without it the composer paints against codex's
    // built-in default and clashes with the surrounding card background.
    const rgb = theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
    const card = await api.createCodexCard(waveId, {
      cwd: values.cwd || undefined,
      prompt: values.prompt || undefined,
      theme: rgb,
    });
    dlog('addCardWithValues', 'codex create DONE', { cardId: card.id });
  } catch (err) {
    console.warn('[Calm] codex create failed:', err);
  }
}

async function addCardOfKind(
  _qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  _type: AddPanelKind,
  theme: 'light' | 'dark',
): Promise<void> {
  // Atomic terminal-card create (#13). One round-trip handles card + linked
  // terminal row + daemon spawn, and emits a single `card.added` carrying
  // the final payload. The pre-#13 wire was a 3-step recipe with mutation
  // suppression + manual invalidate to mask the intermediate `payload=null`
  // state; that whole scaffolding is gone — the bridge picks up the one
  // event and the cache converges naturally.
  //
  // #177 — `theme` is required on the wire (`NewTerminalCardBody.theme`);
  // the kernel writes `term.theme_fg/_bg` on the terminal row in the same
  // transaction and every later spawn for that row stamps the matching
  // `--terminal-fg/-bg` daemon argv.
  try {
    dlog('addCardOfKind', 'createTerminalCard START', { waveId, theme });
    const rgb = theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
    const card = await api.createTerminalCard(waveId, {
      theme: rgb,
    });
    dlog('addCardOfKind', 'createTerminalCard DONE', {
      cardId: card.id,
      payload: card.payload,
    });
  } catch (err) {
    console.warn('[Calm] terminal create failed:', err);
  }
}
