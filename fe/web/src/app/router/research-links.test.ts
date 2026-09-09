import { expect, it } from 'vitest';
import { resolveAppReportLink } from './report-links.ts';

it.each([
  ['copied absolute URL', 'https://app.example:8443/desk/track/study'],
  ['root-relative route', '/desk/track/study'],
  ['irrelevant view queries', '/desk/track/study?panel=cards&market=1&track=other'],
  ['case-normalized authority', 'HTTPS://APP.EXAMPLE:8443/desk/track/study'],
])('resolves %s under the supplied deployment base', (_name, url) => {
  expect(resolveAppReportLink(url, { origin: 'https://app.example:8443', basePath: '/desk' })).toEqual({ trackId: 'study', blockId: null });
});

it('supports the root deployment and canonical default ports', () => {
  expect(resolveAppReportLink('https://app.example:443/track/study', { origin: 'https://app.example', basePath: '/' }))
    .toEqual({ trackId: 'study', blockId: null });
});

it('decodes path and valid anchor once while leaving query parameters out of the target', () => {
  expect(resolveAppReportLink('/next/track/study%252F1?target=%2Fother#b%2Dthesis', { origin: 'http://localhost:5200', basePath: '/next' }))
    .toEqual({ trackId: 'study%2F1', blockId: 'b-thesis' });
  expect(resolveAppReportLink('/next/track/%E7%A0%94%E7%A9%B6#b%252Dthesis', { origin: 'http://localhost:5200', basePath: '/next' }))
    .toEqual({ trackId: '研究', blockId: null });
});

it.each(['#bad/anchor', '#bad%ZZ', '#b#other', '#', ''])('keeps the correct Track when the URL anchor is unusable: %s', hash => {
  expect(resolveAppReportLink(`/next/track/study${hash}`, { origin: 'http://localhost:5200', basePath: '/next' }))
    .toEqual({ trackId: 'study', blockId: null });
});

it.each([
  ['external origin', 'http://other.example:5200/next/track/study'],
  ['different port', 'http://localhost:5201/next/track/study'],
  ['different protocol', 'https://localhost:5200/next/track/study'],
  ['wrong base path', '/other/track/study'],
  ['wrong route', '/next/area/study'],
  ['extra path segment', '/next/track/study/other'],
  ['empty id', '/next/track/'],
  ['malformed escape', '/next/track/%ZZ'],
  ['credential-bearing URL', 'http://user:pass@localhost:5200/next/track/study'],
  ['empty credentials', 'http://@localhost:5200/next/track/study'],
  ['backslash normalization', 'http://localhost:5200\\next/track/study'],
  ['embedded control', 'http://localhost:5200/next/track/stu\tdy'],
  ['protocol-relative URL', '//localhost:5200/next/track/study'],
  ['unknown scheme', 'neige://track/study'],
  ['malformed HTTP URL', 'http:/localhost:5200/next/track/study'],
])('rejects %s', (_name, url) => {
  expect(resolveAppReportLink(url, { origin: 'http://localhost:5200', basePath: '/next' })).toBeNull();
});
