import { useSyncExternalStore } from 'react';
import { createPortal } from 'react-dom';
import { useMobileConversationScope } from '../shell/public.tsx';
import { GuideAnchor } from '../../ui/floating-control/public.ts';
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
  const listeners = new Set<() => void>();
  const lesson = createOnboardingScenario(() => { void client.invalidateQueries(); for (const listener of listeners) listener(); });
  const { transport, workspaceFiles } = lesson;
  const restartHost = document.createElement('span');
  const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
  const guideContent = <DemoRestart host={restartHost} subscribe={subscribe} completed={lesson.completed} />;
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const registry = createCardRegistry(); bootCards(registry);
  const leave = () => window.location.assign('/next/');
  const router = createAppRouter({ transport, unauthorized, client, workspaceFiles, prepareReport: prepareReportDocument,
    initialWorkspaceGuide: true, guideContent, guideFacts: { demo: true }, uiPreferences: createUiPreferences(storage), recentFiles: createRecentFileHistory(storage),
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
            <span ref={node => { if (node !== null) node.appendChild(restartHost); }} />
            <Button type="button" label="退出演示" size="sm" variant="secondary" onClick={leave} />
          </div>
        </aside>
        <div className={styles.workspace}><RouterProvider router={router} /></div>
      </div>
    </ConnectionStatusScope></DatabaseScopeProvider>
  </ThemeProvider></QueryClientProvider>);
}

function DemoRestart({ host, subscribe, completed }: Readonly<{ host: HTMLElement; subscribe: (listener: () => void) => () => void; completed: () => boolean }>) {
  const done = useSyncExternalStore(subscribe, completed);
  const conversation = useMobileConversationScope();
  return createPortal(<GuideAnchor id="demo-complete" disabled={!done || conversation?.conversationOpen === true}>{(anchor, describedBy) => <Button ref={anchor} aria-describedby={describedBy} type="button" label="重新开始" size="sm" variant="ghost" onClick={() => window.location.assign(ONBOARDING_DEMO_URL)} />}</GuideAnchor>, host);
}
