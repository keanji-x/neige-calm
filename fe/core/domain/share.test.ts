import { expect, it } from 'vitest';
import { createShareLinkOperation, shareLinksOperation, revokeShareLinkOperation, parseShareBasePath,
  shareLinkSchema, shareAccessOperation, unlockShareLinkOperation, sharePagePath } from './share.js';

it('separates public link IDs from creation secrets and requires expiry', () => {
  const link = { delegation: { id: 'link', scope: { kind: 'track', id: 't' }, approverUserId: 'u',
    purpose: 'conversation', resourcePolicy: 'current_deployment', createdAt: 1 }, revokedAt: null, expiresAt: 86401 };
  for (const kind of ['track', 'area'] as const) {
    const op = createShareLinkOperation({ kind, id: 'a/b' });
    expect(op.path).toBe(`/api/${kind === 'track' ? 'tracks' : 'areas'}/a%2Fb/share-links`);
    expect(op.body).toEqual({});
    expect(op.responseSchema.parse({ link, token: 'secret' })).toEqual({ link, token: 'secret' });
    expect(op.responseSchema.safeParse({ link }).success).toBe(false);
    expect(shareLinksOperation({ kind, id: 'a/b' }).responseSchema.parse([link])).toEqual([link]);
  }
  expect(shareLinkSchema.safeParse({ ...link, expiresAt: undefined }).success).toBe(false);
  expect(revokeShareLinkOperation('a/b').path).toBe('/api/share-links/a%2Fb');
});
it('decodes locked and scoped access and posts a token only to unlock', () => {
  const op = shareAccessOperation('a/b');
  expect(op.path).toBe('/api/share-links/a%2Fb/access');
  expect(op.responseSchema.parse({ status: 'locked', expiresAt: 10 })).toEqual({ status: 'locked', expiresAt: 10 });
  expect(op.responseSchema.safeParse({ status: 'unlocked', expiresAt: 10 }).success).toBe(false);
  expect(op.responseSchema.safeParse({ status: 'unlocked', scope: { kind: 'all', id: 'x' }, expiresAt: 10 }).success).toBe(false);
  const unlock = unlockShareLinkOperation('a/b', 'secret');
  expect(unlock.path).toBe('/api/share-links/a%2Fb/unlock'); expect(unlock.body).toEqual({ token: 'secret' });
  expect(unlock.responseSchema.parse({ scope: { kind: 'area', id: 'a' }, expiresAt: 10 }).scope).toEqual({ kind: 'area', id: 'a' });
});
it('recognizes all nested production routes without treating their segments as credentials', () => {
  for (const suffix of ['', '/', '/track/t', '/track/t/card/c', '/area/a/new']) {
    expect(parseShareBasePath(`/next/share/a%2Fb${suffix}`)).toEqual({ shareId: 'a/b', basepath: '/next/share/a%2Fb' });
  }
  for (const path of ['/next/share', '/next/share/', '/next/track/t', '/next/share/%GG']) expect(parseShareBasePath(path)).toBeNull();
  expect(sharePagePath('public-id')).toBe('/next/share/public-id');
});
