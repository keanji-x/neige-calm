/* The exact bytes `TrackReportPayload::initial().body` ships, read off `crates/calm-types/src/report/default.md`
   rather than transcribed. Test-only. Functions, not module-level bindings: reading files at import time is
   the module runtime state `architecture/` forbids. */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Closes the contract comment; a blank line then starts the first section. */
const CLOSE = '-->\n\n';

/** The exact body a freshly minted track's report card holds. */
export function initialBody(): string {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../../../..');
  return readFileSync(resolve(repoRoot, 'crates/calm-types/src/report/default.md'), 'utf8');
}

/** `[contract, ...sections]` — the same five slices `split_body` derives, split at the comment close and at each line-initial `# `. */
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
