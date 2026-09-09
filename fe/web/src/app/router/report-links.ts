import { parseReportBlockId, type ReportLinkTarget } from '../../../../core/domain/report.ts';
import { pathFor, routeParamFromPath } from './navigation.ts';

/** Resolve only an actual Track route in this browser's application. The
 * caller supplies deployment context; no host or mount path is baked in. */
export function resolveAppReportLink(destination: string, application: Readonly<{ origin: string; basePath: string }>): ReportLinkTarget | null {
  const value = destination.trim();
  if (!/^https?:\/\//i.test(value) && (!value.startsWith('/') || value.startsWith('//'))) return null;
  if (value.includes('\\') || /^https?:\/\/[^/?#]*@/i.test(value)) return null;
  for (const character of value) {
    const code = character.charCodeAt(0);
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return null;
  }
  try {
    const url = new URL(value, application.origin);
    if (url.origin !== application.origin || url.username !== '' || url.password !== '') return null;
    const base = application.basePath.replace(/\/$/, '');
    const prefix = `${base}${pathFor({ name: 'track', trackId: '' })}`;
    if (!url.pathname.startsWith(prefix)) return null;
    const segment = url.pathname.slice(prefix.length);
    if (segment === '' || segment.includes('/')) return null;
    const trackId = routeParamFromPath(url.pathname.slice(base.length), '/track/');
    if (trackId === undefined) return null;
    let fragment: string | undefined;
    try { fragment = decodeURIComponent(url.hash.slice(1)); } catch { /* Invalid anchors land at the report top. */ }
    return { trackId, blockId: parseReportBlockId(fragment) };
  } catch {
    return null;
  }
}
