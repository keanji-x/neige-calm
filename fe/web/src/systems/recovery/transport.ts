import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { RecoveryAccess, type RecoveryPermit } from '../../../../core/domain/recovery/access.ts';

function authRequest(request: ApiRequest): boolean {
  return (request.method === 'GET' && (request.path === '/api/auth/whoami' || request.path === '/api/version')) ||
    (request.method === 'POST' && (request.path === '/api/auth/login' || request.path === '/api/auth/logout'));
}

/** Separate capability for the identity/version owner; business callers never receive it. */
export function createRecoveryTransports(base: ApiTransportPort, access: RecoveryAccess): Readonly<{
  business: ApiTransportPort; probe: ApiTransportPort;
}> {
  const send = async (request: ApiRequest, permit: RecoveryPermit, probe: boolean): Promise<ApiTransportResponse> => {
    if (probe) {
      if (!authRequest(request) || permit.generation !== access.read().generation) throw new Error('Invalid recovery request');
    } else access.check(permit, request.method !== 'GET');
    const controller = new AbortController();
    const relay = () => controller.abort();
    request.signal?.addEventListener('abort', relay, { once: true });
    if (request.signal?.aborted) relay();
    const unsubscribe = access.subscribe(() => { if (permit.generation !== access.read().generation) relay(); });
    try {
      const response = await base.send({ ...request, signal: controller.signal });
      // Before performApiRequest can decode or broadcast a 401, discard old responses.
      if (controller.signal.aborted || permit.generation !== access.read().generation) throw new Error('Expired recovery response');
      if (!probe) access.check(permit, request.method !== 'GET');
      return response;
    } finally { unsubscribe(); request.signal?.removeEventListener('abort', relay); }
  };
  const admission = {
    capture: () => access.capture(),
    checkpoint: () => {
      const generation = access.read().generation;
      return () => { if (generation !== access.read().generation) throw new Error('Expired response'); };
    },
    scope: (permit: RecoveryPermit): ApiTransportPort => ({
      recovery: { ...admission,
        capture: () => { access.check(permit); return permit; },
        checkpoint: () => () => access.check(permit, false),
        scope: () => admission.scope(permit),
      },
      send: (request) => send(request, permit, false),
    }),
  };
  return {
    business: { recovery: admission, send: (request) => send(request, { generation: access.read().generation }, false) },
    probe: { recovery: { ...admission, capture: () => { throw new Error('Recovery capability cannot admit business writes'); } }, send: (request) => send(request, { generation: access.read().generation }, true) },
  };
}
