import { useEffect, useRef } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  approveMobilePair, createMobileInvitation, readMobileAccess, revokeMobileDevice, setMobileAccess,
  loginPrivateTailnet, logoutPrivateTailnet, type TailnetLoginRequest,
  type MobileInvitation, type MobileAccessStatus,
} from '../../../../core/api/mobile-access.ts';
import type { ApiResult, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createScanEnrollment, cancelScanEnrollment, readScanEnrollmentStatus, type ScanEnrollment } from '../../../../core/api/enrollment.ts';
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
  const [enrollment, setEnrollment] = useState<ScanEnrollment | null>(null);
  const generation = useRef(0);
  const active = useRef(true);
  const acting = useRef(false);
  const client = useQueryClient();
  const observed = useRef<{ origin: string | null; nodeId: string | null } | null>(null);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; generation.current += 1; };
  }, []);
  const query = useQuery({
    queryKey: ['mobile-access'],
    queryFn: async () => valueOf(await readMobileAccess(transport, unauthorized)),
    retry: false,
    refetchInterval: 2000,
  });
  const scanStatus = useQuery({
    queryKey: ['mobile-enrollment-status'],
    queryFn: async () => valueOf(await readScanEnrollmentStatus(transport, unauthorized)),
    enabled: query.data?.provider === 'private-tailnet',
    retry: false,
    refetchInterval: 10_000,
  });
  useEffect(() => {
    const status = query.data;
    if (status === undefined) return;
    const node = status.tailnet;
    const previous = observed.current;
    const changedOrigin = node?.httpsReady && node.origin !== null && previous?.origin != null && node.origin !== previous.origin;
    const changedNode = node?.nodeId != null && previous?.nodeId != null && node.nodeId !== previous.nodeId;
    if (status.provider !== 'private-tailnet' || node?.desiredEnabled === false || changedOrigin || changedNode) {
      generation.current += 1;
      setEnrollment(null);
    }
    if (node !== null) observed.current = {
      origin: node.httpsReady && node.origin !== null ? node.origin : previous?.origin ?? null,
      nodeId: node.nodeId ?? previous?.nodeId ?? null,
    };
  }, [query.data, setEnrollment]);
  useEffect(() => {
    if (enrollment === null) return;
    const timeout = setTimeout(() => setEnrollment(null), Math.max(0, enrollment.pairExpiresAt - Date.now()));
    return () => clearTimeout(timeout);
  }, [enrollment, setEnrollment]);

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
    if (acting.current) return;
    acting.current = true;
    setBusy(true);
    setError(null);
    try { await operation(); await query.refetch(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : 'Connection operation failed'); }
    finally { acting.current = false; if (active.current) setBusy(false); }
  }

  async function createEnrollment() {
    const current = ++generation.current;
    const authority = observed.current;
    setEnrollment(null);
    const created = valueOf(await createScanEnrollment(transport, unauthorized));
    const latest = client.getQueryData<MobileAccessStatus>(['mobile-access']);
    const node = latest?.tailnet;
    const revoked = latest?.provider !== 'private-tailnet' || node?.desiredEnabled !== true
      || (node.httpsReady && node.origin !== null && authority?.origin != null && node.origin !== authority.origin)
      || (node.nodeId !== null && authority?.nodeId != null && node.nodeId !== authority.nodeId);
    if (!active.current || generation.current !== current || revoked) {
      await cancelScanEnrollment(transport, unauthorized, created.enrollmentId);
      return;
    }
    setEnrollment(created);
  }

  function retireEnrollment() { generation.current += 1; setEnrollment(null); }

  return <MobileAccessPane
    status={query.data}
    invitation={invitation}
    enrollment={enrollment}
    cleanup={scanStatus.data?.detail ?? null}
    onCancelEnrollment={() => { const id = enrollment?.enrollmentId; retireEnrollment(); if (id !== undefined) void act(async () => { const result = valueOf(await cancelScanEnrollment(transport, unauthorized, id)); client.setQueryData(['mobile-enrollment-status'], result); }); }}
    login={query.data?.tailnet?.nodeState === 'needs-login' && query.data.tailnet.desiredEnabled ? login : null}
    onLogin={() => { void act(async () => { setLogin(valueOf(await loginPrivateTailnet(transport, unauthorized))); }); }}
    onLogout={() => { retireEnrollment(); void act(async () => { setLogin(null); setInvitation(null); valueOf(await logoutPrivateTailnet(transport, unauthorized)); }); }}
    busy={busy}
    error={error ?? (query.error instanceof Error ? query.error.message : scanStatus.error instanceof Error ? scanStatus.error.message : null)}
    onBack={onBack}
    onRefresh={() => { setError(null); void query.refetch(); void scanStatus.refetch(); }}
    onEnable={() => { void act(async () => { valueOf(await setMobileAccess(transport, unauthorized, true)); }); }}
    onDisable={() => { retireEnrollment(); void act(async () => { valueOf(await setMobileAccess(transport, unauthorized, false)); setInvitation(null); setLogin(null); }); }}
    onCreate={() => { void act(query.data?.provider === 'private-tailnet' ? createEnrollment : async () => { setInvitation(valueOf(await createMobileInvitation(transport, unauthorized))); }); }}
    onApprove={(id) => { void act(async () => { valueOf(await approveMobilePair(transport, unauthorized, id)); setInvitation(null); }); }}
    onRevoke={(id) => { void act(async () => { valueOf(await revokeMobileDevice(transport, unauthorized, id)); }); }}
  />;
}
