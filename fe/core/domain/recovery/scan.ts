/** A single native document input. Never persist this value or reconstruct it
 * from URL/history/presentation state. It carries pairing intent, not identity. */
export type ScanContext = Readonly<{
  generation: number; origin: string; enrollmentId: string; attemptId: string;
  attemptSecret: string; pairTicket: string; deadline: number;
}>;
export type ScanClaim = Readonly<{ enrollmentId: string; attemptId: string; claimId: string }>;
export type ScanRedeem = Readonly<{ enrollmentId: string; attemptId: string; sessionFingerprint: string }>;
export type ScanInput = Readonly<{ kind: 'absent' }> | Readonly<{ kind: 'invalid' }> | Readonly<{ kind: 'scan'; context: ScanContext }>;
function record(value: unknown): value is Record<string, unknown> { return typeof value === 'object' && value !== null && !Array.isArray(value); }
function identifier(value: unknown): value is string { return typeof value === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(value); }
function secret(value: unknown): value is string { return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value); }
export function decodeScanContext(value: unknown, origin: string, now: number): ScanContext | null {
  if (!record(value) || Object.keys(value).length !== 7 || !Number.isSafeInteger(value.generation) ||
    typeof value.generation !== 'number' || value.generation <= 0 || value.origin !== origin || !origin.startsWith('https://') ||
    !identifier(value.enrollmentId) || !identifier(value.attemptId) || !secret(value.attemptSecret) || !secret(value.pairTicket) ||
    !Number.isSafeInteger(value.deadline) || typeof value.deadline !== 'number' || value.deadline <= now || value.deadline - now > 240_000) return null;
  return Object.freeze({ generation: value.generation, origin, enrollmentId: value.enrollmentId, attemptId: value.attemptId,
    attemptSecret: value.attemptSecret, pairTicket: value.pairTicket, deadline: value.deadline });
}
export function decodeScanClaim(value: unknown, context: ScanContext): ScanClaim {
  if (!record(value) || Object.keys(value).length !== 3 || value.enrollmentId !== context.enrollmentId || value.attemptId !== context.attemptId || !identifier(value.claimId)) throw new Error('配对响应与本次扫码不符，请重新扫码。');
  return { enrollmentId: context.enrollmentId, attemptId: context.attemptId, claimId: value.claimId };
}
export function decodeScanRedeem(value: unknown, context: ScanContext): ScanRedeem {
  if (!record(value) || Object.keys(value).length !== 3 || value.enrollmentId !== context.enrollmentId || value.attemptId !== context.attemptId || !secret(value.sessionFingerprint)) throw new Error('配对响应与本次扫码不符，请重新扫码。');
  return { enrollmentId: context.enrollmentId, attemptId: context.attemptId, sessionFingerprint: value.sessionFingerprint };
}
