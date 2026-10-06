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
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import {
  MOBILE_APPROVE_FAILURES, MOBILE_INVITATION_FAILURES, MOBILE_REVOKE_FAILURES,
  MOBILE_STATE_FAILURES, MOBILE_WRITE_TEXT,
} from '../../../../core/domain/mobile-access.ts';
import { MobileAccessPane } from '../../features/settings/mobile-access.tsx';
import { useOperationFeedback, type FailureReading } from '../../ui/operation-feedback/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { useRecoveryMutation } from '../providers/recovery-mutation.ts';

/** One mobile write, given the transport the recovery runner admitted it on; it runs only once the write is sent. */
type MobileWrite = (admitted: ApiTransportPort) => Promise<unknown>;

/** A request's value, or its failure as the rejection the runner and the status queries read. */
async function valueOf<T>(request: Promise<ApiResult<T>>): Promise<T> {
  const result = await request;
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

/** The status reads (`GET /api/mobile/access`, `GET /api/mobile/enrollments`) as the shared read rule says them. */
function statusReadErrorText(error: unknown): string | null {
  return error === null ? null : readErrorText(error, 'Mobile access status could not be read.');
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

  /* Offline, a write is refused at the press and never sent (#2131 class 9); its local effects live inside `MobileWrite`,
     so a refused press leaves the QR, the sign-in link and the scan generation as they were. */
  const write = useRecoveryMutation(transport, { gcTime: 0, mutationFn: (send: MobileWrite, admitted: ApiTransportPort) => send(admitted) });

  /**
   * One write at a time, admitted by the recovery runner and settled by the non-chat runner through the write's table.
   * Whatever it answered, the list is read again, so `done` and `unknown` show what is in effect.
   */
  async function act(send: MobileWrite, read: FailureReading) {
    if (acting.current) return;
    acting.current = true;
    setBusy(true);
    try { await feedback.run(write.mutateAsync(send), read); await client.refetchQueries({ queryKey: ['mobile-access'], exact: true }); }
    finally { acting.current = false; if (active.current) setBusy(false); }
  }

  /**
   * The scan generation fence: a create retires the shown QR and takes a new generation only once it is sent, and its
   * answer lands only while that generation is still the latest and the node it was issued for still serves access.
   * Any other answer is cancelled on the server, never shown.
   */
  async function createEnrollment(admitted: ApiTransportPort) {
    const current = ++generation.current;
    const authority = observed.current;
    setEnrollment(null);
    const created = await valueOf(createScanEnrollment(admitted, unauthorized));
    const latest = client.getQueryData<MobileAccessStatus>(['mobile-access']);
    const node = latest?.tailnet;
    const revoked = latest?.provider !== 'private-tailnet' || node?.desiredEnabled !== true
      || (node.httpsReady && node.origin !== null && authority?.origin != null && node.origin !== authority.origin)
      || (node.nodeId !== null && authority?.nodeId != null && node.nodeId !== authority.nodeId);
    if (!active.current || generation.current !== current || revoked) {
      // A cleanup of a key no screen shows, not a new intent: it goes out even if this write's admission has lapsed.
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
      const id = enrollment?.enrollmentId;
      if (id !== undefined) void act(async (admitted) => {
        retireEnrollment();
        client.setQueryData(['mobile-enrollment-status'], await valueOf(cancelScanEnrollment(admitted, unauthorized, id)));
      }, state);
    }}
    login={query.data?.tailnet?.nodeState === 'needs-login' && query.data.tailnet.desiredEnabled ? login : null}
    onLogin={() => { void act(async (admitted) => { setLogin(await valueOf(loginPrivateTailnet(admitted, unauthorized))); }, state); }}
    onLogout={() => { void act(async (admitted) => { retireEnrollment(); setLogin(null); setInvitation(null); await valueOf(logoutPrivateTailnet(admitted, unauthorized)); }, state); }}
    busy={busy}
    error={feedback.error ?? statusReadErrorText(query.error) ?? statusReadErrorText(scanStatus.error)}
    onBack={onBack}
    onRefresh={() => { feedback.clear(); void query.refetch(); void scanStatus.refetch(); }}
    onEnable={() => { void act((admitted) => valueOf(setMobileAccess(admitted, unauthorized, true)), state); }}
    onDisable={() => { void act(async (admitted) => { retireEnrollment(); await valueOf(setMobileAccess(admitted, unauthorized, false)); setInvitation(null); setLogin(null); }, state); }}
    onCreate={() => { void act(query.data?.provider === 'private-tailnet' ? createEnrollment : async (admitted) => { setInvitation(null); setInvitation(await valueOf(createMobileInvitation(admitted, unauthorized))); },
      writeFailureText(MOBILE_INVITATION_FAILURES, MOBILE_WRITE_TEXT)); }}
    onApprove={(id) => { void act(async (admitted) => { await valueOf(approveMobilePair(admitted, unauthorized, id)); setInvitation(null); },
      writeFailureText(MOBILE_APPROVE_FAILURES, MOBILE_WRITE_TEXT)); }}
    onRevoke={(id) => { void act((admitted) => valueOf(revokeMobileDevice(admitted, unauthorized, id)), writeFailureText(MOBILE_REVOKE_FAILURES, MOBILE_WRITE_TEXT)); }}
  />;
}
