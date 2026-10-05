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
import { ApiError, writeFailureText } from '../../../../core/domain/failure-class.ts';
import {
  MOBILE_APPROVE_FAILURES, MOBILE_INVITATION_FAILURES, MOBILE_READ_FAILURES, MOBILE_READ_TEXT, MOBILE_REVOKE_FAILURES,
  MOBILE_STATE_FAILURES, MOBILE_WRITE_TEXT,
} from '../../../../core/domain/mobile-access.ts';
import { MobileAccessPane } from '../../features/settings/mobile-access.tsx';
import { useOperationFeedback, type FailureReading } from '../../ui/operation-feedback/public.tsx';
import { useState } from '../../ui/state/public.ts';

/** A request's value, or its failure as the rejection the runner and the status queries read. */
async function valueOf<T>(request: Promise<ApiResult<T>>): Promise<T> {
  const result = await request;
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

function readErrorText(error: unknown): string | null {
  return error === null ? null : writeFailureText(MOBILE_READ_FAILURES, MOBILE_READ_TEXT)(error);
}

export function MobileAccessHost({ transport, unauthorized, onBack }: Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onBack: () => void;
}>) {
  const [login, setLogin] = useState<TailnetLoginRequest | null>(null);
  const [busy, setBusy] = useState(false);
  const feedback = useOperationFeedback();
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
    queryFn: () => valueOf(readMobileAccess(transport, unauthorized)),
    retry: false,
    refetchInterval: 2000,
  });
  const scanStatus = useQuery({
    queryKey: ['mobile-enrollment-status'],
    queryFn: () => valueOf(readScanEnrollmentStatus(transport, unauthorized)),
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

  /**
   * One write at a time, settled by the non-chat runner through the write's table. Whatever it answered, the list is
   * read again, so `done` and `unknown` show what is in effect.
   */
  async function act(write: () => Promise<unknown>, read: FailureReading) {
    if (acting.current) return;
    acting.current = true;
    setBusy(true);
    try { await feedback.run(write(), read); await query.refetch(); }
    finally { acting.current = false; if (active.current) setBusy(false); }
  }

  async function createEnrollment() {
    const current = ++generation.current;
    const authority = observed.current;
    setEnrollment(null);
    const created = await valueOf(createScanEnrollment(transport, unauthorized));
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
  const state = writeFailureText(MOBILE_STATE_FAILURES, MOBILE_WRITE_TEXT);

  return <MobileAccessPane
    status={query.data}
    invitation={invitation}
    enrollment={enrollment}
    cleanup={scanStatus.data?.detail ?? null}
    onCancelEnrollment={() => {
      const id = enrollment?.enrollmentId; retireEnrollment();
      if (id !== undefined) void act(async () => { client.setQueryData(['mobile-enrollment-status'], await valueOf(cancelScanEnrollment(transport, unauthorized, id))); }, state);
    }}
    login={query.data?.tailnet?.nodeState === 'needs-login' && query.data.tailnet.desiredEnabled ? login : null}
    onLogin={() => { void act(async () => { setLogin(await valueOf(loginPrivateTailnet(transport, unauthorized))); }, state); }}
    onLogout={() => { retireEnrollment(); void act(async () => { setLogin(null); setInvitation(null); await valueOf(logoutPrivateTailnet(transport, unauthorized)); }, state); }}
    busy={busy}
    error={feedback.error ?? readErrorText(query.error) ?? readErrorText(scanStatus.error)}
    onBack={onBack}
    onRefresh={() => { feedback.clear(); void query.refetch(); void scanStatus.refetch(); }}
    onEnable={() => { void act(() => valueOf(setMobileAccess(transport, unauthorized, true)), state); }}
    onDisable={() => { retireEnrollment(); void act(async () => { await valueOf(setMobileAccess(transport, unauthorized, false)); setInvitation(null); setLogin(null); }, state); }}
    onCreate={() => { void act(query.data?.provider === 'private-tailnet' ? createEnrollment : async () => { setInvitation(null); setInvitation(await valueOf(createMobileInvitation(transport, unauthorized))); },
      writeFailureText(MOBILE_INVITATION_FAILURES, MOBILE_WRITE_TEXT)); }}
    onApprove={(id) => { void act(async () => { await valueOf(approveMobilePair(transport, unauthorized, id)); setInvitation(null); },
      writeFailureText(MOBILE_APPROVE_FAILURES, MOBILE_WRITE_TEXT)); }}
    onRevoke={(id) => { void act(() => valueOf(revokeMobileDevice(transport, unauthorized, id)), writeFailureText(MOBILE_REVOKE_FAILURES, MOBILE_WRITE_TEXT)); }}
  />;
}
