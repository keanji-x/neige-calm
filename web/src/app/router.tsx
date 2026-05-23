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

import { lazy } from 'react';
import {
  createRootRoute,
  createRoute,
  createRouter,
  useParams,
} from '@tanstack/react-router';
import { CalmApp } from '../CalmApp';
import { MissingShell } from './shell';
import { useGo } from './navigation';
import { useTheme } from './theme';
import { LIGHT_THEME_RGB, DARK_THEME_RGB } from '../shared/themeRgb';
import { useTodayTerminal } from '../hooks/useTodayTerminal';
import {
  covesQueryOptions,
  settingsQueryOptions,
  useCovesQuery,
  useCreateWaveMutation,
  useDeleteCardMutation,
  useDeleteCoveMutation,
  useDeleteWaveMutation,
  useOverlaysByKindQuery,
  useUpdateCoveMutation,
  useUpdateWaveMutation,
  useWaveDetailQuery,
  useWavesByCoveQuery,
  waveDetailQueryOptions,
  wavesByCoveQueryOptions,
} from '../api/queries';
import { adaptCard, adaptCove, adaptWave } from '../api/adapt';
import * as api from '../api/calm';
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

const routeTree = rootRoute.addChildren([
  indexRoute,
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
  const createWave = useCreateWaveMutation();
  const updateCove = useUpdateCoveMutation();
  const deleteCove = useDeleteCoveMutation();
  const deleteWave = useDeleteWaveMutation();
  // #177: snapshot the host browser's current theme so each wave-create
  // POST can stamp `theme: { fg, bg }` onto the body. The auto-minted
  // spec card's codex daemon then advertises matching colors on OSC
  // 10/11 — without this, the spec card's TUI paints against codex's
  // built-in default and visually clashes with the surrounding card
  // (same hole the codex-card POST closed for user-created codex
  // cards in PR #193).
  const { resolved: theme } = useTheme();

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
      onCreateWave={async (cId, title) => {
        // #177: stamp theme on the create body so the auto-minted spec
        // card daemon advertises matching colors on OSC 10/11.
        const rgb = theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
        const w = await createWave.mutateAsync({
          cove_id: cId,
          title,
          theme: { fg: rgb.fg, bg: rgb.bg },
        });
        go({ name: 'wave', id: w.id });
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

function WaveComponent() {
  // eslint-disable-next-line no-console
  console.warn('[#177 WaveComponent render]');
  const go = useGo();
  const { waveId } = useParams({ from: waveRoute.id });
  const detailQ = useWaveDetailQuery(waveId);
  const covesQ = useCovesQuery();
  const qc = useQueryClient();
  const updateWave = useUpdateWaveMutation();
  const deleteWave = useDeleteWaveMutation();
  const deleteCard = useDeleteCardMutation();
  // #177 root-cause fix: we deliberately do NOT call `useTheme()` here.
  // Subscribing to ThemeContext in this route component re-renders the
  // whole wave subtree on theme toggle, which trips TanStack Router's
  // `<Match>` Suspense boundary and remounts XtermView — wiping the
  // theme-effect's `prevThemeRef` so the OSC `TerminalThemeUpdate` over
  // the live WS never fires. Instead we read the resolved theme from
  // `document.documentElement.dataset.theme` at click-time inside
  // `onCreateCardWithBody` below. ThemeProvider writes that attribute
  // synchronously on every theme change (see `app/theme.tsx`), so the
  // POST body always carries the latest theme without subscribing.
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
    const adapted = adaptCard(k);
    if (adapted) return { kind: 'card', card: adapted, sort: k.sort };
    return { kind: 'unknown', id: k.id, kernelKind: k.kind, sort: k.sort };
  });

  return (
    <WavePage
      wave={uiWave}
      cove={cove}
      onGo={go}
      onAddCard={async (wId, type) => {
        await addCardOfKind(qc, wId, type);
      }}
      onCreateCardWithBody={async (wId, type, values) => {
        // #177: read the resolved theme at click-time (not via
        // `useTheme()` at render-time — see comment above). The
        // ThemeProvider mirrors `resolved` into `<html data-theme>`
        // synchronously, so this read is always current.
        const theme: 'light' | 'dark' =
          document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light';
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
    return addCardOfKind(qc, waveId, type);
  }
  try {
    dlog('addCardWithValues', 'codex create START', { waveId, values });
    // Atomic codex-card create (#117). One round-trip writes the card row,
    // the linked terminal row, payload (with `terminal_id` + optional
    // `cwd`), AND spawns the codex daemon. Server emits a single
    // `card.added` event carrying the final payload — no intermediate
    // empty-payload flash for the renderer's "Codex is starting…"
    // placeholder to react to.
    //
    // #177: stamp the host browser's current theme RGB onto the body so
    // the daemon's `TerminalModel` can answer codex's OSC 10/11 startup
    // probe with matching colors. Without this the composer paints
    // against codex's built-in default and visually clashes with the
    // card background.
    const rgb =
      theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
    const card = await api.createCodexCard(waveId, {
      cwd: values.cwd || undefined,
      prompt: values.prompt || undefined,
      theme: { fg: rgb.fg, bg: rgb.bg },
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
): Promise<void> {
  // Atomic terminal-card create (#13). One round-trip handles card + linked
  // terminal row + daemon spawn, and emits a single `card.added` carrying
  // the final payload. The pre-#13 wire was a 3-step recipe with mutation
  // suppression + manual invalidate to mask the intermediate `payload=null`
  // state; that whole scaffolding is gone — the bridge picks up the one
  // event and the cache converges naturally.
  try {
    dlog('addCardOfKind', 'createTerminalCard START', { waveId });
    const card = await api.createTerminalCard(waveId, {});
    dlog('addCardOfKind', 'createTerminalCard DONE', {
      cardId: card.id,
      payload: card.payload,
    });
  } catch (err) {
    console.warn('[Calm] terminal create failed:', err);
  }
}
