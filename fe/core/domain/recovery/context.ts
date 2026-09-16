import { createStorageKey } from '../../keys/storage.js';

export function recoveryContextKey() { return createStorageKey('recovery', 'presentation'); }
export function logoutMarkerKey() { return createStorageKey('recovery', 'logout'); }
export type RecoveryPage = Readonly<{ route: string; kind: 'today' | 'track' | 'recipes' | 'settings'; pane: string | null }>;
export type RecoveryScope = Readonly<{ origin: string; userId: string; dbInstanceId: string }>;
export type RecoveryScroll = Readonly<{ region: 'page' | 'board' | 'panel'; top: number; left: number }>;
export type RecoveryContext = Readonly<{ schemaVersion: 1; scope: RecoveryScope; page: RecoveryPage; scroll: readonly RecoveryScroll[] }>;

/** Deliberately excludes titles, bodies, file paths, secrets and arbitrary query strings. */
export function recoveryPage(path: string, search = ''): RecoveryPage {
  let route = '/next/'; let kind: RecoveryPage['kind'] = 'today'; let pane: string | null = null;
  if (/^\/next\/track\/[A-Za-z0-9_-]{1,128}$/.test(path)) {
    route = path; kind = 'track';
    const params = new Map<string, string>();
    for (const pair of search.replace(/^\?/, '').split('&')) {
      const [key, value] = pair.split('=');
      if (key && value && !params.has(key)) params.set(key, value);
    }
    const card = params.get('card'); const panel = params.get('panel'); const from = params.get('from');
    const query: string[] = [];
    if (card && /^[A-Za-z0-9_-]{1,128}$/.test(card)) query.push(`card=${card}`);
    if (panel && /^(outline|cards|tasks|conversations)$/.test(panel)) { pane = panel; query.push(`panel=${panel}`); }
    if (from === 'pages' || from === 'area') query.push(`from=${from}`);
    if (query.length) route += `?${query.join('&')}`;
  } else if (path === '/next/recipes') { route = path; kind = 'recipes'; }
  else if (/^\/next\/settings(?:\/(?:general|network|plugins|appearance|about))?$/.test(path)) {
    route = path; kind = 'settings';
  }
  return { route, kind, pane };
}

export function decodeRecoveryContext(raw: string | null, origin: string): RecoveryContext | null {
  if (raw === null || raw.length > 4096) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== 'object' || value === null) return null;
    const v = value as Partial<RecoveryContext>;
    if (v.schemaVersion !== 1 || v.scope?.origin !== origin || typeof v.scope.userId !== 'string' ||
      typeof v.scope.dbInstanceId !== 'string' || typeof v.page?.route !== 'string') return null;
    const [path, query = ''] = v.page.route.split('?');
    const page = recoveryPage(path, query);
    if (page.route !== v.page.route || page.kind !== v.page.kind || page.pane !== v.page.pane) return null;
    if (!Array.isArray(v.scroll) || v.scroll.length > 3 || v.scroll.some((offset: RecoveryScroll) => !['page', 'board', 'panel'].includes(offset.region) || !Number.isFinite(offset.top) || !Number.isFinite(offset.left) || offset.top < 0 || offset.left < 0 || offset.top > 10_000_000 || offset.left > 10_000_000)) return null;
    return { schemaVersion: 1, scope: v.scope, page, scroll: v.scroll };
  } catch { return null; }
}
