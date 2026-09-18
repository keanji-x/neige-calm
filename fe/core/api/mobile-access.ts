import { z } from 'zod';
import { performApiRequest } from './client.js';
import type { ApiTransportPort } from './types.js';
import type { UnauthorizedChannel } from './unauthorized.js';
import type { MobileStatus, PairingCreated, TailnetLogin } from './generated/wire.js';

const pendingPair = z.object({ id: z.string(), deviceName: z.string(), verificationCode: z.string() });
const device = z.object({ id: z.string(), deviceName: z.string() });
const tailnetSchema = z.object({
  desiredEnabled: z.boolean(),
  phase: z.enum(['disabled', 'starting', 'needs-login', 'needs-approval', 'online', 'degraded', 'failed']),
  processRunning: z.boolean(), childPid: z.number().int().positive().nullable(),
  nodeState: z.enum(['stopped', 'starting', 'needs-login', 'needs-approval', 'online', 'offline']),
  httpsReady: z.boolean(), upstreamReady: z.boolean(), origin: z.string().nullable(),
  dnsName: z.string().nullable(), nodeId: z.string().nullable(), addresses: z.array(z.string()), detail: z.string(),
});
const statusSchema = z.object({
  provider: z.enum(['unavailable', 'funnel', 'private-tailnet']), tailnet: tailnetSchema.nullable(),
  available: z.boolean(), publicUrl: z.string().nullable(),
  pending: z.array(pendingPair), devices: z.array(device),
}).refine((value) => (value.provider === 'private-tailnet') === (value.tailnet !== null), 'Provider status mismatch') satisfies z.ZodType<MobileStatus>;
const invitationSchema = z.object({
  id: z.string(), qrPayload: z.string(),
  qrImage: z.string().startsWith('data:image/svg+xml;base64,'),
  expiresInSeconds: z.number().positive(),
}) satisfies z.ZodType<PairingCreated>;

export type MobileAccessStatus = Readonly<MobileStatus>;
export type MobileInvitation = Readonly<PairingCreated>;

export function readMobileAccess(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, { method: 'GET', path: '/api/mobile/access', responseSchema: statusSchema }, unauthorized);
}

export function setMobileAccess(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, enabled: boolean) {
  return performApiRequest(transport, {
    method: enabled ? 'POST' : 'DELETE', path: '/api/mobile/access', body: {}, responseSchema: statusSchema,
  }, unauthorized);
}

export function createMobileInvitation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, {
    method: 'POST', path: '/api/mobile/pairings', body: {}, responseSchema: invitationSchema,
  }, unauthorized);
}

export function approveMobilePair(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, id: string) {
  return performApiRequest(transport, {
    method: 'POST', path: `/api/mobile/pairings/${encodeURIComponent(id)}/approve`, body: {}, responseSchema: z.void(),
  }, unauthorized);
}

export function revokeMobileDevice(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, id: string) {
  return performApiRequest(transport, {
    method: 'DELETE', path: `/api/mobile/devices/${encodeURIComponent(id)}`, body: {}, responseSchema: z.void(),
  }, unauthorized);
}

export type TailnetLoginRequest = Readonly<TailnetLogin>;
const tailnetLoginSchema = z.object({
  loginUrl: z.url().startsWith('https://login.tailscale.com/'), displayForSeconds: z.number().int().positive(),
}) satisfies z.ZodType<TailnetLogin>;

export function loginPrivateTailnet(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, {
    method: 'POST', path: '/api/mobile/tailnet/login', body: {}, responseSchema: tailnetLoginSchema,
  }, unauthorized);
}

export function logoutPrivateTailnet(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, {
    method: 'POST', path: '/api/mobile/tailnet/logout', body: {}, responseSchema: statusSchema,
  }, unauthorized);
}
