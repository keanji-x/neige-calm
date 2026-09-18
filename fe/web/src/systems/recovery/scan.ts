import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { decodeScanClaim, decodeScanContext, decodeScanRedeem, type ScanContext, type ScanInput } from '../../../../core/domain/recovery/scan.ts';

/** Called only by the bundled app composition, before mounting the session
 * owner. The non-persisted input is removed even when malformed. */
export function takeScanInput(host: Window, origin: string, now: number): ScanInput {
  if (!Object.prototype.hasOwnProperty.call(host, '__NEIGE_SCAN__')) return { kind: 'absent' };
  const holder = host as unknown as Record<string, unknown>;
  const descriptor = Object.getOwnPropertyDescriptor(holder, '__NEIGE_SCAN__');
  if (!Reflect.deleteProperty(holder, '__NEIGE_SCAN__')) return { kind: 'invalid' };
  if (descriptor === undefined || !('value' in descriptor)) return { kind: 'invalid' };
  const context = decodeScanContext(descriptor.value, origin, now);
  return context === null ? { kind: 'invalid' } : { kind: 'scan', context };
}

export type ScanPairingPort = Readonly<{
  claim(context: ScanContext, signal: AbortSignal): Promise<void>;
  redeem(context: ScanContext, signal: AbortSignal): Promise<string>;
}>;

/** A narrow enrollment capability held only by RecoverySession. Business and
 * ordinary recovery transports never receive an enrollment bypass. */
export function createScanPairingPort(base: ApiTransportPort): ScanPairingPort {
  const post = async (path: '/api/mobile/enrollments/claim' | '/api/mobile/enrollments/redeem', body: unknown, signal: AbortSignal): Promise<unknown> => {
    if (signal.aborted) throw new Error('扫码已取消。');
    const result = await base.send({ path, method: 'POST', body, credentials: 'include', signal, headers: { 'Content-Type': 'application/json' } });
    if (signal.aborted) throw new Error('扫码已取消。');
    if (result.status !== 200) throw new Error(result.status === 401 ? '二维码已失效，请在电脑上重新添加手机。' : '配对未完成，请重新扫码。');
    return result.body;
  };
  return {
    claim: async (context, signal) => {
      const result = await post('/api/mobile/enrollments/claim', { enrollmentId: context.enrollmentId, ticket: context.pairTicket,
        deviceName: 'Neige Android', attemptId: context.attemptId, attemptSecret: context.attemptSecret }, signal);
      decodeScanClaim(result, context);
    },
    redeem: async (context, signal) => {
      const result = await post('/api/mobile/enrollments/redeem', { enrollmentId: context.enrollmentId,
        attemptId: context.attemptId, attemptSecret: context.attemptSecret }, signal);
      return decodeScanRedeem(result, context).sessionFingerprint;
    },
  };
}
