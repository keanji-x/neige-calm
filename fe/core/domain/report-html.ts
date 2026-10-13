// The `html` report block (#2585): a self-contained page in the Track workspace, named by a
// workspace-relative `path`. The kernel owns the rule (`calm_types::report_blocks::html`) and
// `test-data/report-html-path-v1.json` pins both sides to the same cases; this reader may accept
// more than the kernel, never less. The path only ever reaches the Track's raw route as a query
// value, which the kernel checks again before it opens anything.

import { z } from 'zod';
import { max2048CodePoints } from './report-table.js';
import { REPORT_FRAME_MAX_HEIGHT, REPORT_FRAME_MIN_HEIGHT } from './report-frame.js';

/** `^[A-Za-z][A-Za-z0-9+.-]*:` — anything a URL parser would read as a scheme. */
const SCHEME = /^[A-Za-z][A-Za-z0-9+.-]*:/;

function hasHostileChar(path: string): boolean {
  for (let index = 0; index < path.length; index += 1) {
    const code = path.charCodeAt(index);
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return true;
    if (code === 0x5c /* \ */ || code === 0x3f /* ? */ || code === 0x23 /* # */) return true;
  }
  return false;
}

/**
 * The one page-name rule (the kernel's `is_html_page_name`): the last segment is `{stem}.html` or
 * `{stem}.htm`, case-insensitive, with a non-empty stem. A bare `.html` is a dotfile, not a page.
 */
export function isHtmlPageName(path: string): boolean {
  const name = (path.split('/').at(-1) ?? '').toLowerCase();
  return ['.html', '.htm'].some((extension) => name.length > extension.length && name.endsWith(extension));
}

/** Unicode `White_Space`, the set Rust's `str::trim` strips (the kernel trims a workspace path before opening it). */
const SURROUNDING_WHITE_SPACE = /^\p{White_Space}|\p{White_Space}$/u;

/**
 * A path the Track raw route serves as a page exactly as named: a page name, and no surrounding
 * whitespace (the route trims it, so such a path would name a different file).
 */
export function isHtmlPagePath(path: string): boolean {
  return !SURROUNDING_WHITE_SPACE.test(path) && isHtmlPageName(path);
}

/**
 * Relative, no `..` segment, no surrounding whitespace, no scheme, `\`, `?`, `#` or control
 * character, and a `{stem}.html`/`.htm` name.
 */
export function isReportHtmlPath(path: string): boolean {
  return path !== ''
    && !path.startsWith('/')
    && !SCHEME.test(path)
    && !hasHostileChar(path)
    && !path.split('/').includes('..')
    && isHtmlPagePath(path);
}

export const htmlBlockPayloadSchema = z.strictObject({
  path: max2048CodePoints(z.string().refine(isReportHtmlPath, { message: 'must be a workspace-relative .html path' })),
  title: max2048CodePoints(z.string()).nullish(),
  height: z.number().min(REPORT_FRAME_MIN_HEIGHT).max(REPORT_FRAME_MAX_HEIGHT).nullish(),
});

export type HtmlBlockPayload = z.infer<typeof htmlBlockPayloadSchema>;
