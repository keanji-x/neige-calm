// TanStack Router setup — code-based (not file-based).
//
// We keep all data fetching exactly where it is (useKernel + pages.tsx
// props) for this milestone; this file only handles "what page renders
// for which URL" plus URL-sync of the route. A later pass will move
// loaders / data-fetching into per-route `loader` functions.
//
// Routes:
//   /                  → TodayPage
//   /cove/$coveId      → CovePage
//   /wave/$waveId      → WavePage
//
// The root route renders <CalmApp /> as a layout shell; CalmApp owns
// Sidebar + TitleBar + kernel state, and emits an <Outlet /> for the
// matched route. That keeps the kernel hook mounted once across nav.

import {
  createRootRoute,
  createRoute,
  createRouter,
  useParams,
} from '@tanstack/react-router';
import { CalmApp } from '../CalmApp';
import { CovePage, TodayPage, WavePage } from '../pages';
import { MissingShell, useCalmShell } from './shell';
import { useGo } from './navigation';

// ---------- Route tree ----------

const rootRoute = createRootRoute({
  component: CalmApp,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: IndexComponent,
});

const coveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/cove/$coveId',
  component: CoveComponent,
});

const waveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/wave/$waveId',
  component: WaveComponent,
});

const routeTree = rootRoute.addChildren([indexRoute, coveRoute, waveRoute]);

export const router = createRouter({
  routeTree,
  // Use HTML5 history; default works fine. We don't need preloading yet.
  defaultPreload: false,
});

// Type registration for the router so `useNavigate` / `Link` are typed.
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}

// ---------- Route page components ----------
//
// Each route component pulls the shared "shell" state from CalmApp via
// `useCalmShell()` (which CalmApp exposes through a context above the
// <Outlet />) and stitches together the right props for the page.
//
// This keeps page bodies (TodayPage / CovePage / WavePage) unchanged —
// they still receive the same props they used to, just sourced from
// route params + the shell context instead of CalmApp's local state.

function IndexComponent() {
  const s = useCalmShell();
  const go = useGo();
  return (
    <TodayPage
      waves={s.waves}
      coves={s.coves}
      onGo={go}
      todayTerminalId={s.todayTerm.today?.terminalId ?? null}
      todayError={s.todayTerm.error}
      onResetTodayTerminal={s.todayTerm.reset}
    />
  );
}

function CoveComponent() {
  const s = useCalmShell();
  const go = useGo();
  const { coveId } = useParams({ from: coveRoute.id });
  const cove = s.coves.find((c) => c.id === coveId) ?? null;
  if (!cove) return <MissingShell label="Cove" onGo={go} />;
  return (
    <CovePage
      cove={cove}
      waves={s.waves.filter((w) => w.coveId === cove.id)}
      onGo={go}
      onCreateWave={async (cId, title) => {
        const w = await s.k.createWave(cId, title);
        go({ name: 'wave', id: w.id });
      }}
      onRenameCove={async (cId, name) => {
        try {
          await s.k.renameCove(cId, name);
        } catch (err) {
          console.warn('[Calm] cove rename failed:', err);
        }
      }}
      onDeleteCove={async (cId) => {
        try {
          await s.k.deleteCove(cId);
          go({ name: 'today' });
        } catch (err) {
          console.warn('[Calm] cove delete failed:', err);
        }
      }}
      onDeleteWave={async (waveId) => {
        try {
          await s.k.deleteWave(waveId);
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
    />
  );
}

function WaveComponent() {
  const s = useCalmShell();
  const go = useGo();
  const { waveId } = useParams({ from: waveRoute.id });
  const wave = s.currentWave(waveId) ?? s.waves.find((w) => w.id === waveId) ?? null;
  if (!wave) return <MissingShell label="Wave" onGo={go} />;
  const cove = s.coves.find((c) => c.id === wave.coveId) ?? null;
  if (!cove) return <MissingShell label="Cove" onGo={go} />;
  return (
    <WavePage
      wave={wave}
      cove={cove}
      onGo={go}
      onAddCard={s.addCard}
      onRemoveCard={(wId, idx) => s.removeCard(wId, idx, waveId)}
      onRenameWave={async (wId, title) => {
        try {
          await s.k.renameWave(wId, title);
        } catch (err) {
          console.warn('[Calm] wave rename failed:', err);
        }
      }}
      onDeleteWave={async (wId) => {
        try {
          await s.k.deleteWave(wId);
          go({ name: 'cove', coveId: cove.id });
        } catch (err) {
          console.warn('[Calm] wave delete failed:', err);
        }
      }}
    />
  );
}
