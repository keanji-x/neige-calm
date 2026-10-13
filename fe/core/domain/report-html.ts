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

/** The last segment is `{stem}.html` or `{stem}.htm` with a non-empty stem; a bare `.html` is a dotfile, not a page. */
function hasHtmlName(path: string): boolean {
  const name = (path.split('/').at(-1) ?? '').toLowerCase();
  return ['.html', '.htm'].some((extension) => name.length > extension.length && name.endsWith(extension));
}

/** Relative, no `..` segment, no scheme, `\`, `?`, `#` or control character, and a `{stem}.html`/`.htm` name. */
export function isReportHtmlPath(path: string): boolean {
  return path !== ''
    && !path.startsWith('/')
    && !SCHEME.test(path)
    && !hasHostileChar(path)
    && !path.split('/').includes('..')
    && hasHtmlName(path);
}

export const htmlBlockPayloadSchema = z.strictObject({
  path: max2048CodePoints(z.string().refine(isReportHtmlPath, { message: 'must be a workspace-relative .html path' })),
  title: max2048CodePoints(z.string()).nullish(),
  height: z.number().min(REPORT_FRAME_MIN_HEIGHT).max(REPORT_FRAME_MAX_HEIGHT).nullish(),
});

export type HtmlBlockPayload = z.infer<typeof htmlBlockPayloadSchema>;
