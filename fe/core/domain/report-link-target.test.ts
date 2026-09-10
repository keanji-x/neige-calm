import { expect, it, vi } from 'vitest';
import { resolveReportLinkTarget } from './report-link-target.js';

it('preserves opaque legacy ids without decoding them', () => {
  for (const id of ['study-1', '研究', 'literal%2Fid', 'nested/study?raw#id', ' id with spaces ']) {
    expect(resolveReportLinkTarget(id)).toEqual({ trackId: id, blockId: null });
  }
  expect(resolveReportLinkTarget('  ')).toBeNull();
});

it('uses existing report-link citation and anchor behavior before consulting the browser', () => {
  const browser = vi.fn(() => null);
  expect(resolveReportLinkTarget('neige://wave/study%252F1#b-thesis', browser)).toEqual({ trackId: 'study%2F1', blockId: 'b-thesis' });
  expect(resolveReportLinkTarget('neige://wave/%ZZ#invalid/anchor', browser)).toEqual({ trackId: '%ZZ', blockId: null });
  expect(browser).not.toHaveBeenCalled();
});

it('requires a browser resolver for app URLs', () => {
  for (const url of ['https://app.example/next/track/study', '/next/track/study']) {
    expect(resolveReportLinkTarget(url)).toBeNull();
  }
});

it('delegates copied web destinations without decoding their path or fragment', () => {
  const target = { trackId: 'study%2F1', blockId: 'b-thesis' };
  const browser = vi.fn(() => target);
  expect(resolveReportLinkTarget(' https://app.example/next/track/study%252F1#b-thesis ', browser)).toEqual(target);
  expect(browser).toHaveBeenCalledExactlyOnceWith('https://app.example/next/track/study%252F1#b-thesis');
});

it('keeps rejected URL inputs inert instead of treating them as ids', () => {
  const browser = vi.fn(() => null);
  for (const value of ['https://elsewhere.example/next/track/study', '/wrong/path', 'http:broken', 'https:/broken', 'http//broken', 'neige://track/study',
    'javascript:alert(1)', 'file:///tmp/study', 'mailto:study@example.com', '#b-thesis', '../track/study', './track/study', 'bad\\path']) {
    expect(resolveReportLinkTarget(value, browser), value).toBeNull();
  }
});

it.each(['https ://app.example/next/track/study', 'neige ://wave/study', '://app.example/next/track/study'])(
  'rejects a malformed scheme delimiter instead of opening it as an id: %s', value => {
    expect(resolveReportLinkTarget(value), value).toBeNull();
  },
);
