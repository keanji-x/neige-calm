import { describe, expect, it } from 'vitest';

import type { CardWire } from './track.js';
import { readTrackReport, TRACK_REPORT_CARD_KIND } from './report.js';

/** The `window` block's fields as `readTrackReport` reads them. Its `src` cases are the shared
 *  kernel fixture, checked in `tools/report-view/window-src.test.ts`. */
function firstBlock(payload: unknown) {
  const card: CardWire = {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'window', rev: 1, payload }] },
    deletable: false, created_at: 0, updated_at: 0,
  };
  return readTrackReport([card])?.blocks?.[0];
}

const SRC = '/api/plugins/desktop/ws/apps/chrome/stream';

describe('window blocks', () => {
  it('reads a window block with every optional field', () => {
    expect(firstBlock({ src: SRC, title: 'Chrome', height: 720 })).toEqual({
      id: 'b-1', kind: 'window', payload: { src: SRC, title: 'Chrome', height: 720 },
    });
    expect(firstBlock({ src: SRC })).toEqual({ id: 'b-1', kind: 'window', payload: { src: SRC } });
  });

  it.each([
    ['no src', {}],
    ['a src off the allowlist', { src: '/api/plugins/desktop/ws/.. ' }],
    ['a field the kernel refuses', { src: SRC, plugin: 'desktop' }],
    ['a title that is not a string', { src: SRC, title: 7 }],
    ['a height that is not a number', { src: SRC, height: '720' }],
    ['a height below 120', { src: SRC, height: 50 }],
  ])('degrades a window block with %s to unsupported', (_label, payload) => {
    expect(firstBlock(payload)).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'window' });
  });
});
