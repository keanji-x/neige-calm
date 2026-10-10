// The `src`/`path` rules of the report blocks that load a URL on this origin: `app` and `preview`
// take any same-origin path (the renderer re-checks the origin); `window` takes only the kernel's
// allowlisted plugin socket path. A payload the kernel accepts is exactly one this renders.

import { z } from 'zod';
import { max2048CodePoints } from './report-table.js';

/**
 * A same-origin absolute path: a leading `/`, not `//`, and no backslashes (browsers normalize `\`
 * to `/` inside a URL). The `app` renderer re-asserts the origin: two checks for the one block that
 * loads someone else's markup.
 */
export function sameOriginPath() {
  return max2048CodePoints(z.string()
  .regex(/^\/(?!\/)[^\\]*$/, { message: 'must be a same-origin absolute path' })
  .refine((value) => {
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return false;
    }
    return true;
  }, { message: 'must not contain control characters' }));
}

/**
 * The `window` block's `src` rule (#2530), verbatim the kernel's `WINDOW_SRC_PATTERN`
 * (`calm_types::report_blocks::window`); `test-data/window-src-v1.json` pins both to the same cases.
 * An allowlist: every segment is `[A-Za-z0-9_-]+`, so nothing in it is a byte the browser's URL
 * parser strips, decodes or resolves (spaces, U+2028, dots, `%`, backslash, `?`, `#`).
 */
export const WINDOW_SRC_PATTERN = '^/api/plugins/[a-z0-9][a-z0-9.-]{1,63}/ws/[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*$';

/** No flags: `$` is the end of the input, and no `.`, `\s` or Unicode class can widen the match. */
export function windowStreamSrc() {
  return max2048CodePoints(z.string().regex(new RegExp(WINDOW_SRC_PATTERN), { message: 'must be a plugin window-stream path' }));
}
