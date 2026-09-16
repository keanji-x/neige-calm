import { useEffect } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  approveMobilePair, createMobileInvitation, readMobileAccess, revokeMobileDevice, setMobileAccess,
  loginPrivateTailnet, logoutPrivateTailnet, type TailnetLoginRequest,
  type MobileInvitation,
} from '../../../../core/api/mobile-access.ts';
import type { ApiResult, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { MobileAccessPane } from '../../features/settings/mobile-access.tsx';
import { useState } from '../../ui/state/public.ts';

function valueOf<T>(result: ApiResult<T>): T {
  if (result.status === 'failed') throw new Error(result.error.message);
  return result.value;
}

export function MobileAccessHost({ transport, unauthorized, onBack }: Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onBack: () => void;
}>) {
  const [login, setLogin] = useState<TailnetLoginRequest | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [invitation, setInvitation] = useState<MobileInvitation | null>(null);
  const query = useQuery({
    queryKey: ['mobile-access'],
    queryFn: async () => valueOf(await readMobileAccess(transport, unauthorized)),
    retry: false,
    refetchInterval: 2000,
  });
  useEffect(() => {
    if (invitation === null) return;
    const timeout = setTimeout(() => setInvitation(null), invitation.expiresInSeconds * 1000);
    return () => clearTimeout(timeout);
  }, [invitation, setInvitation]);

  useEffect(() => {
    if (login === null) return;
    const timeout = setTimeout(() => setLogin(null), login.displayForSeconds * 1000);
    return () => clearTimeout(timeout);
  }, [login, setLogin]);

  async function act(operation: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try { await operation(); await query.refetch(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : 'Connection operation failed'); }
    finally { setBusy(false); }
  }

  return <MobileAccessPane
    status={query.data}
    invitation={invitation}
    login={query.data?.tailnet?.nodeState === 'needs-login' && query.data.tailnet.desiredEnabled ? login : null}
    onLogin={() => { void act(async () => { setLogin(valueOf(await loginPrivateTailnet(transport, unauthorized))); }); }}
    onLogout={() => { void act(async () => { setLogin(null); setInvitation(null); valueOf(await logoutPrivateTailnet(transport, unauthorized)); }); }}
    busy={busy}
    error={error ?? (query.error instanceof Error ? query.error.message : null)}
    onBack={onBack}
    onRefresh={() => { setError(null); void query.refetch(); }}
    onEnable={() => { void act(async () => { valueOf(await setMobileAccess(transport, unauthorized, true)); }); }}
    onDisable={() => { void act(async () => { valueOf(await setMobileAccess(transport, unauthorized, false)); setInvitation(null); setLogin(null); }); }}
    onCreate={() => { void act(async () => { setInvitation(valueOf(await createMobileInvitation(transport, unauthorized))); }); }}
    onApprove={(id) => { void act(async () => { valueOf(await approveMobilePair(transport, unauthorized, id)); setInvitation(null); }); }}
    onRevoke={(id) => { void act(async () => { valueOf(await revokeMobileDevice(transport, unauthorized, id)); }); }}
  />;
}
