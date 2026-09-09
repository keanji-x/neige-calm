// Preview data enters at Neige's transport seam; the router, shell, Track page,
// report renderer, outline, table, file viewer, and styles are production imports.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel } from '../../../fe/core/api/unauthorized.ts';
import { createAppRouter } from '../../../fe/web/src/app/router/public.tsx';
import { ThemeProvider } from '../../../fe/web/src/app/theme/public.tsx';
import { createCardHost, createCardRegistry } from '../../../fe/web/src/systems/cards/public.ts';
import { bootCards } from '../../../fe/web/src/app/cards.ts';
import { createInvestmentPreviewTransport } from './native-transport';
import { mountLivePortfolio } from './live-app';
import { PORTFOLIO_MODE_KEY } from './portfolio-keys';

const requestedMode = new URL(location.href).searchParams.get('market');
if (requestedMode !== null) localStorage.setItem(PORTFOLIO_MODE_KEY, requestedMode === '1' ? 'live' : 'demo');
const liveMode = localStorage.getItem(PORTFOLIO_MODE_KEY) === 'live';
const modeLabel = document.getElementById('portfolio-mode-label');
if (modeLabel) modeLabel.textContent = liveMode ? 'Market data 模式 · 在侧栏选择组合 Track' : '示例数据';
document.getElementById('portfolio-demo-mode')?.addEventListener('click', () => { location.href = '/next/?market=0'; });
document.getElementById('portfolio-live-mode')?.addEventListener('click', () => { location.href = '/next/?market=1'; });
if (!liveMode && (location.pathname === '/' || location.pathname === '/next/' || location.pathname === '/next')) {
  history.replaceState(null, '', '/next/track/portfolio');
}
const root = document.getElementById('root');
if (!root) throw new Error('Missing preview root');
let dispose: () => void;
if (liveMode) {
  dispose = mountLivePortfolio(root);
} else {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const registry = createCardRegistry();
  bootCards(registry);
  const router = createAppRouter({
    transport: createInvestmentPreviewTransport(), client,
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }),
    cards: { registry, host: createCardHost(registry) }, onSignOut: () => undefined,
  });
  const app = createRoot(root);
  app.render(<QueryClientProvider client={client}>
    <ThemeProvider storage={window.localStorage}><RouterProvider router={router}/></ThemeProvider>
  </QueryClientProvider>);
  dispose = () => { app.unmount(); client.clear(); };
}
if (import.meta.hot) import.meta.hot.dispose(() => dispose());
