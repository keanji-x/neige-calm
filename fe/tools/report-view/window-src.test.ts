import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { readTrackReport, TRACK_REPORT_CARD_KIND } from '../../core/domain/report.js';
import { WINDOW_SRC_PATTERN, windowSrcRegex } from '../../core/domain/report-src.js';
import type { CardWire } from '../../core/domain/track.js';

/** The one `window` src rule, shared with the kernel (`crates/calm-types/src/report_blocks/window_tests.rs`). */
const fixture = JSON.parse(readFileSync(new URL('../../../test-data/window-src-v1.json', import.meta.url), 'utf8')) as {
  pattern: string;
  accept: string[];
  refuse: string[];
};

function readsAsWindow(src: string): boolean {
  const card: CardWire = {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'window', rev: 1, payload: { src } }] },
    deletable: false, created_at: 0, updated_at: 0,
  };
  return readTrackReport([card])?.blocks?.[0]?.kind === 'window';
}

describe('window src conformance shared with the kernel', () => {
  it('uses the kernel pattern verbatim', () => {
    expect(WINDOW_SRC_PATTERN).toBe(fixture.pattern);
    // `RegExp#source` escapes `/`; compare against the same construction of the shared text.
    expect(windowSrcRegex().source).toBe(new RegExp(fixture.pattern).source);
    expect(windowSrcRegex().flags).toBe('');
  });

  it('reads every accept case as a window block', () => {
    expect(fixture.accept.filter((src) => !readsAsWindow(src))).toEqual([]);
  });

  it('degrades every refuse case to unsupported', () => {
    expect(fixture.refuse.filter((src) => readsAsWindow(src))).toEqual([]);
  });
});
