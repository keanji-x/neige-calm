import { parseReportLink, type ReportLinkTarget } from './report.js';

/** Browser-owned parsing of a URL into a route in the current application. */
export type ReportAppLinkResolver = (destination: string) => ReportLinkTarget | null;

/** Table link fields accept saved ids, wave citations, or validated app URLs.
 * URL-shaped values never fall back to literal ids when validation fails. */
export function resolveReportLinkTarget(destination: string, resolveAppLink?: ReportAppLinkResolver): ReportLinkTarget | null {
  const value = destination.trim();
  if (value === '') return null;
  const citation = parseReportLink(value);
  if (citation !== null) return citation;
  if (/^https?:\/\//i.test(value) || value.startsWith('/')) return resolveAppLink?.(value) ?? null;
  if (value.includes('://') || /^[a-z][a-z0-9+.-]*(?::|\/\/)/i.test(value) || /^(?:\.\.?\/|[?#])/.test(value) || value.includes('\\')) return null;
  // Bare ids are opaque: preserve literal percent escapes and whitespace.
  return { trackId: destination, blockId: null };
}
