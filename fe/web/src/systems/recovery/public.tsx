import { useSyncExternalStore } from 'react';
import type { RecoveryContext, RecoveryScroll } from '../../../../core/domain/recovery/context.ts';
import type { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
export function useRecoveryState(access: RecoveryAccess) { return useSyncExternalStore(access.subscribe, access.read, access.read); }
/** Listeners only wake the session owner; none infer authorization. */
export function observeRecoveryLifecycle(owner: Readonly<{
  pause(): void; resume(): void; remember(path: string, search: string, scroll: readonly RecoveryScroll[]): void;
  takePresentation(): RecoveryContext | null;
}>): () => void {
  const regionNode = (region: RecoveryScroll['region']) => region === 'page'
    ? document.querySelector<HTMLElement>('[data-nc-track-page]') : region === 'board'
      ? document.querySelector<HTMLElement>('[data-nc-card-board]') : document.querySelector<HTMLElement>('[data-nc-panel]');
  let pending: RecoveryContext | null = null;
  const applied = new Set<string>();
  const restore = () => {
    const candidate = owner.takePresentation();
    if (candidate !== null) { pending = candidate; applied.clear(); }
    if (pending === null || pending.page.route.split('?')[0] !== window.location.pathname) return;
    for (const offset of pending.scroll) {
      if (applied.has(offset.region)) continue;
      const node = regionNode(offset.region);
      if (!node) continue;
      node.scrollTop = offset.top; node.scrollLeft = offset.left;
      applied.add(offset.region);
    }
    if (pending.scroll.every(offset => applied.has(offset.region))) pending = null;
  };
  const observer = new MutationObserver(restore);
  observer.observe(document.body, { childList: true, subtree: true });
  const remember = () => {
    restore();
    const scroll: RecoveryScroll[] = [];
    for (const region of ['page', 'board', 'panel'] as const) {
      const node = regionNode(region);
      if (node) scroll.push({ region, top: Math.min(10_000_000, Math.max(0, node.scrollTop)), left: Math.min(10_000_000, Math.max(0, node.scrollLeft)) });
    }
    owner.remember(window.location.pathname, window.location.search, scroll);
  };
  const visibility = () => { remember(); if (document.hidden) owner.pause(); else owner.resume(); };
  const online = () => owner.resume();
  const offline = () => { owner.pause(); owner.resume(); };
  document.addEventListener('visibilitychange', visibility);
  window.addEventListener('online', online); window.addEventListener('offline', offline);
  window.addEventListener('pagehide', visibility); window.addEventListener('pageshow', online);
  const timer = setInterval(remember, 1000);
  return () => {
    clearInterval(timer); observer.disconnect(); remember(); document.removeEventListener('visibilitychange', visibility);
    window.removeEventListener('online', online); window.removeEventListener('offline', offline);
    window.removeEventListener('pagehide', visibility); window.removeEventListener('pageshow', online);
  };
}

/** Raw platform reachability; consumers must combine it with their own authority.
 * The subscriber owns these listeners and must release them on disposal. */
export function observeOnlineStatus(listener: (online: boolean) => void): () => void {
  const online = () => listener(true); const offline = () => listener(false);
  window.addEventListener('online', online); window.addEventListener('offline', offline);
  return () => { window.removeEventListener('online', online); window.removeEventListener('offline', offline); };
}
