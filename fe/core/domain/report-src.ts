// The `src`/`path` rules of the blocks that load a same-origin URL. The bounds mirror the kernel's
// validators in `calm_types::report_blocks`, so a payload the kernel accepts is exactly one this renders.

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
 * The `window` block's `src` (#2530): `/api/plugins/{plugin id}/ws/{path}` with a non-empty path,
 * no query, no fragment, and no dot segment in any spelling (`.`, `..`, `%2e`, `.%2E`, …), because
 * the browser resolves dot segments away before it opens the socket. The kernel's
 * `WINDOW_SRC_PATTERN` is this expression.
 */
const WINDOW_SRC_PATTERN = /^(?!.*\/(?:\.|%2[eE]){1,2}(?:\/|$))\/api\/plugins\/[a-z0-9][a-z0-9.-]{1,63}\/ws\/[^?#\\]+$/;

export function windowStreamSrc() {
  return sameOriginPath().regex(WINDOW_SRC_PATTERN, { message: 'must be a plugin window-stream path' });
}
