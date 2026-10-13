import { describe, expect, it } from 'vitest';

import type { CardWire } from './track.js';
import { readTrackReport, TRACK_REPORT_CARD_KIND } from './report.js';

/** The `html` block's fields as `readTrackReport` reads them. Its `path` cases are the shared
 *  kernel fixture, checked in `tools/report-view/html-path.test.ts`. */
function firstBlock(payload: unknown) {
  const card: CardWire = {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'html', rev: 1, payload }] },
    deletable: false, created_at: 0, updated_at: 0,
  };
  return readTrackReport([card])?.blocks?.[0];
}

const PATH = 'docs/architecture/overview.html';

describe('html blocks', () => {
  it('reads an html block with every optional field', () => {
    expect(firstBlock({ path: PATH, title: 'Architecture', height: 640 })).toEqual({
      id: 'b-1', kind: 'html', payload: { path: PATH, title: 'Architecture', height: 640 },
    });
    expect(firstBlock({ path: PATH })).toEqual({ id: 'b-1', kind: 'html', payload: { path: PATH } });
  });

  it.each([
    ['no path', {}],
    ['an absolute path', { path: '/docs/overview.html' }],
    ['a parent segment', { path: 'docs/../overview.html' }],
    ['a URL', { path: 'https://calm.example/overview.html' }],
    ['another extension', { path: 'docs/overview.svg' }],
    ['a field the kernel refuses', { path: PATH, src: PATH }],
    ['a title that is not a string', { path: PATH, title: 7 }],
    ['a height below 120', { path: PATH, height: 119 }],
    ['a height above 2000', { path: PATH, height: 2001 }],
  ])('degrades an html block with %s to unsupported', (_label, payload) => {
    expect(firstBlock(payload)).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'html' });
  });
});
