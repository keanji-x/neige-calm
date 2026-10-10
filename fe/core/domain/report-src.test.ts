import { describe, expect, it } from 'vitest';

import type { CardWire } from './track.js';
import { readTrackReport, TRACK_REPORT_CARD_KIND } from './report.js';

/** The `window` block as `readTrackReport` reads it: the same cases the kernel's validator pins. */
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
  it.each([
    SRC,
    '/api/plugins/dev-neige-market/ws/s',
    '/api/plugins/a.b/ws/s',
    '/api/plugins/desktop/ws/.hidden/a..b/...',
    '/api/plugins/desktop/ws/a%2eb/%2e%2e%2e',
  ])('reads a window block at %s', (src) => {
    expect(firstBlock({ src, title: 'Chrome', height: 720 })).toEqual({
      id: 'b-1', kind: 'window', payload: { src, title: 'Chrome', height: 720 },
    });
  });

  it.each([
    ['another route', '/apps/screener'],
    ['the prefix not at the start', '/x/api/plugins/desktop/ws/s'],
    ['another plugin sub-route', '/api/plugins/desktop/http/s'],
    ['an empty stream path', '/api/plugins/desktop/ws/'],
    ['an invalid plugin id', '/api/plugins/Desktop/ws/s'],
    ['an absolute URL', 'wss://calm.example/api/plugins/desktop/ws/s'],
    ['a protocol-relative URL', '//calm.example/api/plugins/desktop/ws/s'],
    ['a backslash', '/api/plugins/desktop/ws/a\\b'],
    ['a control character', '/api/plugins/desktop/ws/a\nb'],
    ['a query', '/api/plugins/desktop/ws/s?x=1'],
    ['a fragment', '/api/plugins/desktop/ws/s#top'],
    ['a raw parent segment', '/api/plugins/desktop/ws/../../terminals/1'],
    ['a raw current segment', '/api/plugins/desktop/ws/./s'],
    ['a trailing parent segment', '/api/plugins/desktop/ws/s/..'],
    ['an encoded parent segment', '/api/plugins/desktop/ws/%2e%2e/s'],
    ['an upper-case encoded segment', '/api/plugins/desktop/ws/%2E/s'],
    ['a mixed parent segment', '/api/plugins/desktop/ws/.%2E/s'],
    ['a trailing encoded segment', '/api/plugins/desktop/ws/s/%2e'],
  ])('degrades a window block with %s to unsupported', (_label, src) => {
    expect(firstBlock({ src })).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'window' });
  });

  it.each([
    ['a field the kernel refuses', { src: SRC, plugin: 'desktop' }],
    ['a title that is not a string', { src: SRC, title: 7 }],
    ['a height that is not a number', { src: SRC, height: '720' }],
    ['a height below 120', { src: SRC, height: 50 }],
  ])('degrades a window block with %s to unsupported', (_label, payload) => {
    expect(firstBlock(payload)).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'window' });
  });
});
