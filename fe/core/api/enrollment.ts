import { z } from 'zod';
import { performApiRequest } from './client.js';
import type { ApiTransportPort } from './types.js';
import type { UnauthorizedChannel } from './unauthorized.js';
import type { EnrollmentCreated, EnrollmentCleanup } from './generated/wire.js';

const enrollmentSchema = z.strictObject({
  enrollmentId: z.string().regex(/^[A-Za-z0-9_-]{1,128}$/),
  qrPayload: z.string().startsWith('neige-enroll:v2:').max(2048),
  qrImage: z.string().startsWith('data:image/svg+xml;base64,'),
  authKeyExpiresAt: z.number().int().positive(),
  pairExpiresAt: z.number().int().positive(),
}).refine((value) => value.pairExpiresAt <= value.authKeyExpiresAt) satisfies z.ZodType<EnrollmentCreated>;
const cleanupSchema = z.strictObject({
  pendingCleanup: z.number().int().nonnegative(), detail: z.string(),
}) satisfies z.ZodType<EnrollmentCleanup>;

export type ScanEnrollment = Readonly<EnrollmentCreated>;

export function readScanEnrollmentStatus(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, {
    method: 'GET', path: '/api/mobile/enrollments', responseSchema: cleanupSchema,
  }, unauthorized);
}

export function createScanEnrollment(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return performApiRequest(transport, {
    method: 'POST', path: '/api/mobile/enrollments', body: {}, responseSchema: enrollmentSchema,
  }, unauthorized);
}

export function cancelScanEnrollment(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, id: string) {
  return performApiRequest(transport, {
    method: 'DELETE', path: `/api/mobile/enrollments/${encodeURIComponent(id)}`, body: {}, responseSchema: cleanupSchema,
  }, unauthorized);
}
