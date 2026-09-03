/*
 * The exact bytes `TrackReportPayload::initial().body` ships (#1185 §4.4 G).
 *
 * Test-only. A hand-written fixture proves the front end can hide *a* comment;
 * it cannot prove it hides *the* comment the kernel actually emits. The two
 * differ in the ways that matter — the real contract is 30-odd lines, spans
 * several blank lines, and contains `<preview URL>`, which react-markdown sees
 * as one more raw HTML node. So we read the shipped fragments off disk instead
 * of transcribing them.
 *
 * The concatenation mirrors `crates/calm-types/src/track_report.rs`, which pins
 * its own shape in `initial_body_is_the_default_structural_skeleton`.
 *
 * Everything here is a function, not a module-level binding: reading files at
 * import time is side-effectful and would run even for tests that never use it.
 */
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Closes the contract comment; a blank line then starts the first section. */
const CLOSE = '-->\n\n';
const SECTIONS = '# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n';

function fragment(name: string): string {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');
  return readFileSync(resolve(repoRoot, 'crates/calm-types/src', name), 'utf8');
}

/** The exact body a freshly minted track's report card holds. */
export function initialBody(): string {
  return (
    fragment('track_report_contract_rules.md') +
    fragment('track_report_section_rules.md') +
    CLOSE +
    SECTIONS
  );
}

/**
 * `[contract, ...sections]` — the same five slices `split_body` derives, split
 * at the comment close and at each line-initial `# `.
 */
export function splitInitialBody(): string[] {
  const body = initialBody();
  const close = body.indexOf(CLOSE);
  if (close < 0) throw new Error('the shipped contract fragments no longer close the comment');
  const contract = body.slice(0, close + CLOSE.length);
  const sections = body
    .slice(close + CLOSE.length)
    .split(/^(?=# )/m)
    .filter((s) => s.length > 0);
  return [contract, ...sections];
}
