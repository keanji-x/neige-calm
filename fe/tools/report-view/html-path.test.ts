import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { readTrackReport, TRACK_REPORT_CARD_KIND } from '../../core/domain/report.js';
import type { CardWire } from '../../core/domain/track.js';

/** The one `html` path rule, shared with the kernel (`crates/calm-types/src/report_blocks/html_tests.rs`). */
const fixture = JSON.parse(readFileSync(new URL('../../../test-data/report-html-path-v1.json', import.meta.url), 'utf8')) as {
  accept: string[];
  refuse: string[];
};

function readsAsHtml(path: string): boolean {
  const card: CardWire = {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'html', rev: 1, payload: { path } }] },
    deletable: false, created_at: 0, updated_at: 0,
  };
  return readTrackReport([card])?.blocks?.[0]?.kind === 'html';
}

describe('html path conformance shared with the kernel', () => {
  it('reads every accept case as an html block', () => {
    expect(fixture.accept.length).toBeGreaterThan(4);
    expect(fixture.accept.filter((path) => !readsAsHtml(path))).toEqual([]);
  });

  // The reader may accept more than the kernel; today it accepts exactly as much.
  it('degrades every refuse case to unsupported', () => {
    expect(fixture.refuse.length).toBeGreaterThan(19);
    expect(fixture.refuse.filter((path) => readsAsHtml(path))).toEqual([]);
  });
});
