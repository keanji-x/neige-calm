// TanStack Router setup — code-based (not file-based).
//
// Routes:
//   /                  → TodayPage
//   /area/$areaId      → AreaPage
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
// Route loaders prime the relevant TanStack Query cache entries via
// `queryClient.ensureQueryData(...)` using the same `{ queryKey, queryFn }`
// factories exported from `api/queries.ts`, so cache shape stays in lock-step
// with the hook call sites. The wave/area loaders intentionally do this
// without blocking the route commit: selection feedback (URL commit + Sidebar
// active highlight) is instant, and the route component owns its brief in-page
// loading state. The parallel prefetch usually fills the cache before the lazy
// chunk finishes mounting, so spinner flashes stay rare.

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
  areasQueryOptions,
  settingsQueryOptions,
  useAreasQuery,
  useDeleteCardMutation,
  useDeleteAreaMutation,
  useDeleteWaveMutation,
  useOverlaysByKindQuery,
  useUpdateAreaMutation,
  useUpdateWaveMutation,
  useWaveDetailQuery,
  useWavesByAreaQuery,
  waveDetailQueryOptions,
  wavesByAreaQueryOptions,
} from '../api/queries';
import { adaptCard, adaptArea, adaptWave } from '../api/adapt';
import * as api from '../api/calm';
import { DARK_THEME_RGB, LIGHT_THEME_RGB } from '../api/themeRgb';
import { useQueryClient, useQueries } from '@tanstack/react-query';
import { queryKeys } from '../api/queries';
import { queryClient } from './providers';
import { dlog } from '../util/debug';
import type { Area, Wave, WaveCardSlot } from '../types';
import type { AddPanelKind } from '../shared/components/AddPanel';
import { getEntry } from '../cards/registry';
import type { CardCreateStrategy, CardKindClaim } from '../cards/registry';

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
const AreaPage = lazy(() =>
  import('../pages/Area').then((m) => ({ default: m.AreaPage })),
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
  // Today fans out to per-area wave lists on the page itself; we
  // conservatively prefetch only the areas list here. The area → waves
  // fan-out stays lazy (the page uses `useQueries`) so a slow area
  // doesn't block the calendar.
  loader: () => queryClient.ensureQueryData(areasQueryOptions()),
  component: IndexComponent,
});

const areaRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/area/$areaId',
  loader: ({ params }) => {
    // Non-blocking: prime the cache but do NOT await, so the route commits
    // immediately and the sidebar's active-row highlight is instant.
    // AreaComponent renders with an empty wave list until wavesQ resolves.
    // `.catch` keeps a fetch failure (404/5xx/offline) from becoming an
    // unhandled rejection; the error is still recorded on the query so the
    // component can surface it.
    void queryClient
      .ensureQueryData(wavesByAreaQueryOptions(params.areaId))
      .catch(() => {});
  },
  component: AreaComponent,
});

const waveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/wave/$waveId',
  loader: ({ params }) => {
    // Non-blocking: prime the cache but do NOT await, so the route commits
    // immediately and the sidebar's active-row highlight is instant.
    // WaveComponent renders its own loading state (returns null while
    // detailQ.isLoading) until the primed query resolves.
    // `.catch` keeps a fetch failure (404/5xx/offline) from becoming an
    // unhandled rejection; the error is still recorded on the query so the
    // component can surface it (MissingShell / empty state).
    void queryClient
      .ensureQueryData(waveDetailQueryOptions(params.waveId))
      .catch(() => {});
  },
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
  areaRoute,
  waveRoute,
  settingsRoute,
]);

// `basepath` mirrors Vite's `base: '/calm/'` (see vite.config.ts) so URLs
// in the browser actually read `/calm/area/$id` rather than `/area/$id`.
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
  const areasQ = useAreasQuery();
  // Belt-and-suspenders for issue #175 — see CalmApp.tsx for the same
  // filter. The server already hides `kind='system'` from
  // `GET /api/areas` by default, but the second layer of defence keeps
  // the system area out of Today's calendar/clock fan-out as well.
  const kernelAreas = (areasQ.data ?? []).filter((c) => c.kind === 'user');

  // Today's calendar + clock want a flat wave list across all areas.
  // One query per area keeps cache granularity sensible (a wave moving
  // between areas invalidates only the two affected lists).
  const waveQueries = useQueries({
    queries: kernelAreas.map((c) => ({
      queryKey: queryKeys.wavesInArea(c.id),
      queryFn: () => api.wavesInArea(c.id),
    })),
  });

  // Workspace-wide wave overlays — fed into adaptWave so the Sidebar's
  // status indicators ("waiting on you" / "running") are accurate for
  // every wave, not just whichever wave the user has opened. eventBridge
  // invalidates this snapshot on overlay.set/.deleted and on wave/area
  // deletes (where the kernel may not cascade individual events).
  const waveOverlaysQ = useOverlaysByKindQuery('wave');
  const overlaysByWaveId = new Map<string, typeof waveOverlaysQ.data>();
  for (const o of waveOverlaysQ.data ?? []) {
    if (o.entity_kind !== 'wave') continue;
    const cur = overlaysByWaveId.get(o.entity_id);
    if (cur) cur.push(o);
    else overlaysByWaveId.set(o.entity_id, [o]);
  }

  const areas: Area[] = kernelAreas.map(adaptArea);
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
      areas={areas}
      onGo={go}
      todayTerminalId={todayTerm.today?.terminalId ?? null}
      todayError={todayTerm.error}
      onResetTodayTerminal={todayTerm.reset}
    />
  );
}

function AreaComponent() {
  const go = useGo();
  const { areaId } = useParams({ from: areaRoute.id });
  const areasQ = useAreasQuery();
  const wavesQ = useWavesByAreaQuery(areaId);
  const updateArea = useUpdateAreaMutation();
  const deleteArea = useDeleteAreaMutation();
  const deleteWave = useDeleteWaveMutation();
  const updateWave = useUpdateWaveMutation();

  const kernelArea = areasQ.data?.find((c) => c.id === areaId);
  if (!kernelArea) {
    // While the areas list is loading, we don't know if the area exists.
    // Show the calm "Connecting…" shell rather than flashing a missing
    // state. CalmApp already renders LoadingShell for the initial fetch,
    // but a hard-refresh on /area/:id can land here before cache primes.
    if (areasQ.isLoading) return null;
    return <MissingShell label="Area" onGo={go} />;
  }
  const area = adaptArea(kernelArea);
  const waves: Wave[] = (wavesQ.data ?? []).map((w) => adaptWave(w, []));

  return (
    <AreaPage
      area={area}
      waves={waves}
      onGo={go}
      onWaveCreated={(wave) => {
        // Issue #250 PR 3 — the NewTaskForm inside AreaPage owns the
        // wave-create POST end-to-end (cwd + area auto-inference +
        // theme stamping + folder-conflict surfacing). All this
        // callback needs to do is navigate. The cwd-empty stopgap
        // from PR 2 is gone — the form refuses to submit without a
        // valid absolute path.
        go({ name: 'wave', id: wave.id });
      }}
      onRenameArea={async (cId, name) => {
        try {
          await updateArea.mutateAsync({ id: cId, body: { name } });
        } catch (err) {
          console.warn('[Calm] area rename failed:', err);
        }
      }}
      onDeleteArea={async (cId) => {
        try {
          await deleteArea.mutateAsync(cId);
          go({ name: 'today' });
        } catch (err) {
          console.warn('[Calm] area delete failed:', err);
        }
      }}
      onDeleteWave={async (waveId) => {
        try {
          await deleteWave.mutateAsync({ id: waveId, areaId: area.id });
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
      onPinWave={async (waveId, pin) => {
        await updateWave.mutateAsync({
          id: waveId,
          body: { pinned_at: pin ? Date.now() : null },
        });
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
  const areasQ = useAreasQuery();
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

  const detail = detailQ.data;
  // Wave detail is the source of truth for "does this wave exist?".
  // `detailQ.data` may be a keepPreviousData placeholder for the
  // previously-viewed wave while THIS wave's detail is still fetching — the
  // non-blocking route loader commits the URL before data lands. Treat an
  // absent OR mismatched (stale-placeholder) detail as "loading this wave"
  // so we never render the previous wave under this wave's URL. Only a
  // settled miss (no data, not loading/fetching) is a truly missing wave.
  if (!detail || detail.wave.id !== waveId) {
    if (!detail && !detailQ.isLoading && !detailQ.isFetching) {
      return <MissingShell label="Wave" onGo={go} />;
    }
    return null;
  }
  const kernelArea = areasQ.data?.find((c) => c.id === detail.wave.area_id);
  if (!kernelArea) {
    if (areasQ.isLoading) return null;
    return <MissingShell label="Area" onGo={go} />;
  }
  const area = adaptArea(kernelArea);
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
      area={area}
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
          await deleteWave.mutateAsync({ id: wId, areaId: area.id });
          go({ name: 'area', areaId: area.id });
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
    />
  );
}

/**
 * Schema-driven card create. The Wave page hands us the kind + the
 * SchemaForm values; we look up the right kernel sequence per kind.
 *
 * Today zero-config entries and schema-backed entries both flow through here.
 * Other kinds
 * (`plugin:*` / `ui://*`) come through their own create path via the
 * plugin host; they're not menu-driven from the AddPanel.
 */
export async function addCardWithValues(
  qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  type: AddPanelKind,
  values: Record<string, string>,
  theme: 'light' | 'dark',
): Promise<void> {
  const entry = getEntry(type);
  if (!entry) return addCardOfKind(qc, waveId, type, theme);
  let input: unknown;
  try {
    input = entry.addPanel?.createSchema?.parse?.(values) ?? values;
  } catch (err) {
    console.warn(
      `[Calm] ${createWarnKind(entry)} create rejected invalid input:`,
      err,
    );
    throw err;
  }
  await createFromEntry(qc, waveId, entry, input, theme);
}

export class CatalogCreateNotImplemented extends Error {
  constructor() {
    super('CatalogCreateNotImplemented');
  }
}

export class KernelMintedOnlyCreateNotAllowed extends Error {
  constructor() {
    super('KernelMintedOnlyCreateNotAllowed');
  }
}

interface RouterCreateContractEntry {
  type: unknown;
  claim?: CardKindClaim;
  create?: { mode: CardCreateStrategy<unknown>['mode'] };
}

export function assertRouterCreateAllowed(entry: RouterCreateContractEntry): void {
  if (entry.create?.mode === 'catalog') {
    throw new CatalogCreateNotImplemented();
  }
  if (entry.create?.mode === 'kernel-minted-only') {
    throw new KernelMintedOnlyCreateNotAllowed();
  }
}

function createWarnKind(entry: RouterCreateContractEntry): string {
  return entry.claim?.mode === 'exact' ? entry.claim.kind : String(entry.type);
}

function isCreateContractError(err: unknown): boolean {
  if (
    err instanceof CatalogCreateNotImplemented ||
    err instanceof KernelMintedOnlyCreateNotAllowed
  ) {
    return true;
  }
  if (!(err instanceof Error)) return false;
  return /^(MissingCreateStrategy|GenericCreateRequiresExactClaim|EntryMissingMetadata|DuplicateExactClaim|DuplicatePrefixClaim)\(/.test(
    err.message,
  );
}

async function createFromEntry(
  qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  entry: NonNullable<ReturnType<typeof getEntry>>,
  input: unknown,
  theme: 'light' | 'dark',
): Promise<void> {
  if (!entry.create) {
    throw new Error(`MissingCreateStrategy(${entry.type})`);
  }

  try {
    assertRouterCreateAllowed(entry);
    const rgb = theme === 'dark' ? DARK_THEME_RGB : LIGHT_THEME_RGB;
    let result: { cardId: string; raw?: unknown };
    if (entry.create.mode === 'generic') {
      if (entry.claim?.mode !== 'exact') {
        throw new Error(`GenericCreateRequiresExactClaim(${entry.type})`);
      }
      const title = (input as { title?: string }).title || undefined;
      const card = await api.createCard(waveId, {
        kind: entry.claim.kind,
        title,
        payload: entry.create.buildPayload(input as never),
      });
      result = { cardId: card.id, raw: card };
    } else if (entry.create.mode === 'atomic') {
      result = await entry.create.submit(waveId, input as never, {
        themeRgb: rgb,
      });
    } else {
      assertRouterCreateAllowed(entry);
      throw new Error(`MissingCreateStrategy(${entry.type})`);
    }
    await qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
    dlog('createFromEntry', 'DONE', { type: entry.type, cardId: result.cardId });
  } catch (err) {
    if (isCreateContractError(err)) throw err;
    console.warn(`[Calm] ${createWarnKind(entry)} create failed:`, err);
    throw err;
  }
}

async function addCardOfKind(
  qc: ReturnType<typeof useQueryClient>,
  waveId: string,
  type: AddPanelKind,
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
  const entry = getEntry(type);
  if (!entry) return;
  await createFromEntry(qc, waveId, entry, {}, theme);
}
