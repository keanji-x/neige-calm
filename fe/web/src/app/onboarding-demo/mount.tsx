import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createHashHistory, RouterProvider } from '@tanstack/react-router';
import { Badge } from '@astryxdesign/core/Badge';
import { Button } from '@astryxdesign/core/Button';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createCardHost, createCardRegistry } from '../../systems/cards/public.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { bootCards } from '../cards.ts';
import { createAppRouter } from '../router/public.tsx';
import { createUiPreferences, DatabaseScopeProvider } from '../providers/ui-preferences.tsx';
import { createRecentFileHistory } from '../providers/recent-files.ts';
import { ConnectionStatusScope } from '../providers/connection-status.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { ONBOARDING_DEMO_URL } from '../onboarding-entry.ts';
import { createOnboardingScenario } from './scenario.ts';
import styles from './demo.module.css';

/** A complete alternate composition: no production session, storage, event bridge or network adapter. */
export function mountOnboardingDemo(root: HTMLElement): void {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const { transport, workspaceFiles } = createOnboardingScenario(() => { void client.invalidateQueries(); });
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const registry = createCardRegistry(); bootCards(registry);
  const leave = () => window.location.assign('/next/');
  const router = createAppRouter({ transport, unauthorized, client, workspaceFiles, prepareReport: prepareReportDocument,
    initialWorkspaceGuide: true, uiPreferences: createUiPreferences(storage), recentFiles: createRecentFileHistory(storage),
    cards: { registry, host: createCardHost(registry) }, onSignOut: leave });
  const history = createHashHistory();
  // Every document visit starts a fresh lesson; hash navigation cannot lose the Demo selector.
  history.replace('/next/track/demo-home');
  router.update({ history });
  createRoot(root).render(<QueryClientProvider client={client}><ThemeProvider storage={storage}><DepthFeedback />
    <DatabaseScopeProvider id="onboarding-demo"><ConnectionStatusScope value={{ connected: true, label: '演示模式', detail: '只使用页面内的预置数据，不连接真实后端。' }}>
      <div className={styles.demo}>
        <aside className={styles.banner} aria-label="演示模式">
          <div className={styles.identity}><Badge variant="neutral" label="Demo" /><span className={styles.sentence}>预置练习 · 不连接 AI，不修改文件</span></div>
          <div className={styles.actions}>
            <Button type="button" label="重新开始" size="sm" variant="ghost" onClick={() => window.location.assign(ONBOARDING_DEMO_URL)} />
            <Button type="button" label="退出演示" size="sm" variant="secondary" onClick={leave} />
          </div>
        </aside>
        <div className={styles.workspace}><RouterProvider router={router} /></div>
      </div>
    </ConnectionStatusScope></DatabaseScopeProvider>
  </ThemeProvider></QueryClientProvider>);
}
