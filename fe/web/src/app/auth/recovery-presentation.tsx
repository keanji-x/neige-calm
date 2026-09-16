import { Icon } from '@astryxdesign/core/Icon';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { recoveryPage } from '../../../../core/domain/recovery/context.ts';
import type { RecoveryState } from '../../../../core/domain/recovery/access.ts';
import styles from './recovery-presentation.module.css';
export function RecoveryPresentation() {
  const page = recoveryPage(window.location.pathname, window.location.search);
  const title = page.kind === 'track' ? 'Track' : page.kind === 'recipes' ? 'Recipes' : page.kind === 'settings' ? 'Settings' : 'Today';
  return <div className={styles.frame} data-nc-recovery-page={page.kind}>
    <nav aria-label="恢复页面导航">
      <MobileHeader title={title} level={1} leading={<a href="http://tauri.localhost/" className={styles.menu} aria-label="连接设置"><Icon icon="menu" size="md" color="inherit" /></a>} />
    </nav>
    <main className={styles.main}>
      <section className={styles.placeholder} aria-label="等待恢复页面内容">
        <p>{page.pane === 'cards' ? '等待恢复终端' : '连接恢复后显示页面内容'}</p>
        <div className={styles.line} /><div className={styles.line} /><div className={styles.line} />
      </section><a href="http://tauri.localhost/">返回连接页</a>
    </main>
  </div>;
}
export function RecoveryStatus({ state, retry }: Readonly<{ state: RecoveryState; retry(): void }>) {
  const label = state.phase === 'connected' ? '已连接' : state.phase === 'syncing' ? '正在同步' :
    state.phase === 'offline' ? '离线 · 正在重试' : state.phase === 'login' ? '需要重新登录' :
      state.phase === 'update' ? '需要更新' : '正在恢复连接';
  return <details className={styles.status} data-nc-recovery-status={state.phase}>
    <summary><span role="status" aria-live="polite">{label}</span></summary>
    <div className={styles.details}>
      {state.phase !== 'connected' && <p>恢复期间只读，已显示的内容可能过时。</p>}
      {state.detail && <p>{state.detail}</p>}
      {state.failedAt !== null && <p>最近失败：{new Date(state.failedAt).toLocaleTimeString()}</p>}
      {state.retryAt !== null && <p>下次重试：{new Date(state.retryAt).toLocaleTimeString()}</p>}
      {['recovering', 'offline', 'paused'].includes(state.phase) && <button type="button" onClick={retry}>立即重试</button>}
      <a href="http://tauri.localhost/">连接设置</a>
    </div>
  </details>;
}
