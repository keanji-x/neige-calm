import { QueryClient } from '@tanstack/react-query';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel } from '../../../fe/core/api/unauthorized.ts';
import { ProductionApp } from '../../../fe/web/src/app/auth/production-app.tsx';
import { LoginPage } from '../../../fe/web/src/features/auth/login-page/public.tsx';
import { loginWithTransport } from '../../../fe/web/src/app/auth/login.ts';
import { createFetchTransport } from '../../../fe/web/src/app/providers/transport.ts';
import { runOperation, serverVersionOperation, logoutOperation, queryKeys } from '../../../fe/web/src/app/providers/queries.ts';
import { createAppRouter } from '../../../fe/web/src/app/router/public.tsx';
import { createCardHost, createCardRegistry } from '../../../fe/web/src/systems/cards/public.ts';
import { bootCards } from '../../../fe/web/src/app/cards.ts';
import { createLiveMarketTransport } from './live-market';
import { attachChartBridge } from './chart-bridge';
import { SELECTED_PORTFOLIO_KEY } from './portfolio-keys';

export function mountLivePortfolio(root: HTMLElement) {
  const base = createFetchTransport();
  const unauthorized = createUnauthorizedChannel({ enqueue: callback => queueMicrotask(callback) });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  client.setQueryDefaults(queryKeys.trackDetail('').slice(0, 1), { refetchInterval: 30_000 });
  const live = createLiveMarketTransport(base, (id, result) => bridge.publish(id, result), {
    current: localStorage.getItem(SELECTED_PORTFOLIO_KEY),
    save: id => localStorage.setItem(SELECTED_PORTFOLIO_KEY, id),
    clear: () => localStorage.removeItem(SELECTED_PORTFOLIO_KEY),
  });
  const bridge = attachChartBridge(id => live.snapshots.get(id));
  const registry = createCardRegistry(); bootCards(registry);
  const runtime = {
    fetchVersion: () => runOperation(base, serverVersionOperation(), unauthorized),
    reload: () => window.location.reload(), deleteDatabase: (name: string) => { indexedDB.deleteDatabase(name); },
    idbDatabaseName: 'neige-portfolio-preview', storage: window.localStorage,
  };
  const cursorStore = { clear: live.clear };
  const router = createAppRouter({ transport: live.transport, unauthorized, client,
    cards: { registry, host: createCardHost(registry) },
    onSignOut: () => { void runOperation(base, logoutOperation(), unauthorized).finally(() => { live.clear(); client.clear(); window.location.reload(); }); },
  });
  const app = createRoot(root);
  app.render(<ProductionApp transport={base} unauthorized={unauthorized} client={client} runtime={runtime}
    cursorStore={cursorStore} router={router}
    renderLogin={() => <LoginPage login={(username, password) => loginWithTransport(base, username, password)} reload={() => { location.href = '/next/?market=1'; }}/>}
    renderError={retry => <main><p>暂时无法连接 Neige，请检查连接地址后重试。</p><button type="button" onClick={retry}>重试</button></main>}/>);
  return () => { bridge.dispose(); live.clear(); app.unmount(); client.clear(); };
}
