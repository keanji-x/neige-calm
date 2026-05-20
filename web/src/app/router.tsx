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
import { suppressCardEvents } from './eventBridge';
import type { Cove, Wave, WaveCardSlot } from '../types';
import type { AddPanelKind } from '../shared/components/AddPanel';
import { parsePluginItemType } from '../shared/components/AddPanel';

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
  const kernelCoves = covesQ.data ?? [];

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
        const w = await createWave.mutateAsync({ cove_id: cId, title });
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
    const adapted = adaptCard(k);
    if (adapted) return { kind: 'card', card: adapted };
    return { kind: 'unknown', id: k.id, kernelKind: k.kind };
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
        await addCardWithValues(qc, wId, type, values);
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
): Promise<void> {
  if (type !== 'codex') {
    // Falls through to the default "no-config" pathway. The AddPanel
    // shouldn't surface a schema form for kinds without `createSchema`,
    // so this is defensive only.
    return addCardOfKind(qc, waveId, type);
  }
  const release = suppressCardEvents(waveId);
  try {
    dlog('addCardWithValues', 'codex create START', { waveId, values });
    // Two-step: create an empty-payload codex card row, then have the
    // codex route spawn the PTY daemon and stamp `terminal_id` back into
    // the payload. The frontend never pre-populates the payload here —
    // doing so risks racing the server's `card_update` and stomping the
    // terminal_id field on the next refetch.
    const card = await api.createCard(waveId, {
      kind: 'codex',
      payload: {},
    });
    await api.createCodex(card.id, {
      cwd: values.cwd || undefined,
    });
    dlog('addCardWithValues', 'codex create DONE', { cardId: card.id });
  } catch (err) {
    console.warn('[Calm] codex create failed:', err);
  } finally {
    release();
    void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
  }
}

async function addCardOfKind(
  qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  type: AddPanelKind,
): Promise<void> {
  // Plugin entry: dispatch via the M2 tool-call create path. The kernel
  // invokes the plugin's `creator_tool`, extracts `_meta.ui.resourceUri`
  // from the result, and writes a Card row with `kind = <ui://...>`.
  const plugin = parsePluginItemType(type);
  if (plugin) {
    const release = suppressCardEvents(waveId);
    try {
      await api.createCard(waveId, {
        via_tool_call: {
          plugin_id: plugin.pluginId,
          tool_name: plugin.toolName,
          arguments: {},
        },
      });
    } catch (err) {
      console.warn('[Calm] plugin card create failed:', err);
    } finally {
      release();
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
    }
    return;
  }

  // Suppress this wave's WS card events for the duration of the 3-step
  // create. Without this, the kernel's intermediate emissions (card.added
  // with payload=null, before the terminal_id patch) would race the
  // mutation chain through EventBridge → invalidate → refetch and the UI
  // would render the half-built card before snapping to the final state.
  // See PR #12 + issue #13 for the underlying CRUD-API design problem.
  const releaseSuppression = suppressCardEvents(waveId);
  try {
    dlog('addCardOfKind', 'step 1: createCard START', { waveId });
    const card = await api.createCard(waveId, { kind: 'terminal' });
    dlog('addCardOfKind', 'step 1: createCard DONE', { cardId: card.id, payload: card.payload });
    dlog('addCardOfKind', 'step 2: createTerminal START');
    const term = await api.createTerminal(card.id, {});
    dlog('addCardOfKind', 'step 2: createTerminal DONE', { termId: term.id });
    dlog('addCardOfKind', 'step 3: updateCard START');
    await api.updateCard(card.id, { payload: { terminal_id: term.id } });
    dlog('addCardOfKind', 'step 3: updateCard DONE');
  } catch (err) {
    console.warn('[Calm] terminal create failed:', err);
  } finally {
    releaseSuppression();
    // Single, post-flow refresh — the only WS-equivalent the UI sees for
    // this mutation. Wave detail refetches once with the fully-formed
    // card (terminal_id present), so no intermediate-state flash.
    dlog('addCardOfKind', 'MANUAL invalidate (post-flow)', { waveId });
    void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
  }
}
