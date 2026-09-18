import { describe, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createScanPairingPort, takeScanInput } from './scan.ts';
function context() { return { generation: 2, origin: 'https://neige.tail.example', enrollmentId: 'invite', attemptId: 'attempt', attemptSecret: 'a'.repeat(64), pairTicket: 'b'.repeat(64), deadline: 100_000 }; }
describe('scan pairing capability', () => {
  it('takes and deletes only the native one-shot property, never URL input', () => {
    const host = { location: { hash: '#neige-enroll:v2:fake' } } as unknown as Window;
    expect(takeScanInput(host, context().origin, 1_000)).toEqual({ kind: 'absent' });
    Object.defineProperty(host, '__NEIGE_SCAN__', { value: context(), configurable: true });
    expect(takeScanInput(host, context().origin, 1_000)).toEqual({ kind: 'scan', context: context() });
    expect(takeScanInput(host, context().origin, 1_000)).toEqual({ kind: 'absent' });
  });
  it('uses only exact claim/redeem endpoints with cookies and cancellation', async () => {
    const send = vi.fn<(request: ApiRequest) => Promise<ApiTransportResponse>>().mockResolvedValueOnce({ status: 200, statusText: 'OK', body: { enrollmentId: 'invite', attemptId: 'attempt', claimId: 'claim' } })
      .mockResolvedValueOnce({ status: 200, statusText: 'OK', body: { enrollmentId: 'invite', attemptId: 'attempt', sessionFingerprint: 'c'.repeat(64) } });
    const controller = new AbortController(); const port = createScanPairingPort({ send });
    await port.claim(context(), controller.signal);
    expect(await port.redeem(context(), controller.signal)).toBe('c'.repeat(64));
    expect(send.mock.calls.map(([request]) => request.path)).toEqual(['/api/mobile/enrollments/claim', '/api/mobile/enrollments/redeem']);
    for (const [request] of send.mock.calls) expect(request).toMatchObject({ credentials: 'include', signal: controller.signal, method: 'POST' });
    expect(send.mock.calls[0]?.[0].body).toEqual({ enrollmentId: 'invite', ticket: 'b'.repeat(64), deviceName: 'Neige Android', attemptId: 'attempt', attemptSecret: 'a'.repeat(64) });
  });
  it('discards even a successful late response after cancellation', async () => {
    let resolve!: (value: ApiTransportResponse) => void;
    const send = vi.fn(() => new Promise<ApiTransportResponse>(done => { resolve = done; }));
    const controller = new AbortController();
    const promise = createScanPairingPort({ send }).redeem(context(), controller.signal);
    controller.abort(); resolve({ status: 200, statusText: 'OK', body: { enrollmentId: 'invite', attemptId: 'attempt', sessionFingerprint: 'c'.repeat(64) } });
    await expect(promise).rejects.toThrow('取消');
  });
});
