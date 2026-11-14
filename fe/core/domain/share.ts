/** Share management and cookie-backed access contracts. Secrets never appear in URLs. */
import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import type { ShareLink, ShareLinkCreated, ShareScope, ShareAccessStatus, UnlockedShareLink } from '../api/generated/wire.js';

export const shareScopeSchema: z.ZodType<ShareScope> = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('track'), id: z.string().min(1) }),
  z.object({ kind: z.literal('area'), id: z.string().min(1) }),
]);
export const shareLinkSchema: z.ZodType<ShareLink> = z.object({
  delegation: z.object({ id: z.string().min(1), scope: shareScopeSchema, approverUserId: z.string(),
    purpose: z.literal('conversation'), resourcePolicy: z.literal('current_deployment'), createdAt: z.number() }),
  revokedAt: z.number().nullable(), expiresAt: z.number(),
});
export const shareLinkCreatedSchema: z.ZodType<ShareLinkCreated> = z.object({ link: shareLinkSchema, token: z.string().min(1) });
export const shareAccessStatusSchema: z.ZodType<ShareAccessStatus> = z.discriminatedUnion('status', [
  z.object({ status: z.literal('locked'), expiresAt: z.number() }),
  z.object({ status: z.literal('unlocked'), scope: shareScopeSchema, expiresAt: z.number() }),
]);
export const unlockedShareLinkSchema: z.ZodType<UnlockedShareLink> = z.object({ scope: shareScopeSchema, expiresAt: z.number() });
function managementPath(scope: ShareScope): string {
  return `/api/${scope.kind === 'track' ? 'tracks' : 'areas'}/${encodeURIComponent(scope.id)}/share-links`;
}
export function shareLinksOperation(scope: ShareScope): ApiOperation<ShareLink[]> {
  return { method: 'GET', path: managementPath(scope), responseSchema: z.array(shareLinkSchema) };
}
export function createShareLinkOperation(scope: ShareScope): ApiOperation<ShareLinkCreated> {
  return { method: 'POST', path: managementPath(scope), body: {}, responseSchema: shareLinkCreatedSchema };
}
export function revokeShareLinkOperation(id: string): ApiOperation<ShareLink> {
  return { method: 'DELETE', path: `/api/share-links/${encodeURIComponent(id)}`, responseSchema: shareLinkSchema };
}
export function createTrackShareLinkOperation(id: string) { return createShareLinkOperation({ kind: 'track', id }); }
export function createAreaShareLinkOperation(id: string) { return createShareLinkOperation({ kind: 'area', id }); }
export function listTrackShareLinksOperation(id: string) { return shareLinksOperation({ kind: 'track', id }); }
export function listAreaShareLinksOperation(id: string) { return shareLinksOperation({ kind: 'area', id }); }
export function deleteTrackShareLinkOperation(id: string) { return revokeShareLinkOperation(id); }
export function deleteAreaShareLinkOperation(id: string) { return revokeShareLinkOperation(id); }
export function shareAccessOperation(id: string): ApiOperation<ShareAccessStatus> {
  return { method: 'GET', path: `/api/share-links/${encodeURIComponent(id)}/access`, responseSchema: shareAccessStatusSchema };
}
export function unlockShareLinkOperation(id: string, token: string): ApiOperation<UnlockedShareLink> {
  return { method: 'POST', path: `/api/share-links/${encodeURIComponent(id)}/unlock`, body: { token }, responseSchema: unlockedShareLinkSchema };
}
export function sharePagePath(id: string): string { return `/next/share/${encodeURIComponent(id)}`; }
/** Accept every nested production route, but reject missing or malformed share IDs. */
export function parseShareBasePath(pathname: string): Readonly<{ shareId: string; basepath: string }> | null {
  const match = /^\/next\/share\/([^/]+)(?:\/|$)/.exec(pathname);
  if (!match?.[1]) return null;
  try {
    const shareId = decodeURIComponent(match[1]);
    return { shareId, basepath: sharePagePath(shareId) };
  } catch { return null; }
}
