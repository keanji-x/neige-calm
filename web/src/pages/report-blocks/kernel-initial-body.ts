/*
 * The exact bytes `TrackReportPayload::initial().body` ships (#1185 §4.4 G).
 *
 * Test-only. A hand-written fixture proves the front end can hide *a* comment;
 * it cannot prove it hides *the* comment the kernel actually emits. The two
 * differ in the ways that matter — the real contract is 30-odd lines, spans
 * several blank lines, and contains `<preview URL>`, which react-markdown sees
 * as one more raw HTML node. So we read the shipped file off disk instead of
 * transcribing it.
 *
 * The file is `crates/calm-types/src/report/default.md`, the single source of
 * `initial()` since #1635 S2b (its line 1 is the `<!-- neige:contract … -->`
 * header, then the prose contract comment, then the four H1s); the Rust side
 * pins its shape in `initial_body_is_the_default_structural_skeleton`.
 *
 * Everything here is a function, not a module-level binding: reading files at
 * import time is side-effectful and would run even for tests that never use it.
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Closes the contract comment; a blank line then starts the first section. */
const CLOSE = '-->\n\n';

/** The exact body a freshly minted track's report card holds. */
export function initialBody(): string {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');
  return readFileSync(resolve(repoRoot, 'crates/calm-types/src/report/default.md'), 'utf8');
}

/**
 * `[contract, ...sections]` — the same five slices `split_body` derives, split
 * at the comment close and at each line-initial `# `. Block 0 begins with the
 * header line and ends with the prose comment's close.
 */
export function splitInitialBody(): string[] {
  const body = initialBody();
  const close = body.indexOf(CLOSE);
  if (close < 0) throw new Error('the shipped default.md no longer closes the contract comment');
  const contract = body.slice(0, close + CLOSE.length);
  const sections = body
    .slice(close + CLOSE.length)
    .split(/^(?=# )/m)
    .filter((s) => s.length > 0);
  return [contract, ...sections];
}
