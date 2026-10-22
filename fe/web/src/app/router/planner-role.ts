// An existing Track's Planner role, picked in its Planner conversation's composer (#2648): the value is the server's
// `admin_granted_at`, `can_admin` decides whether it may change, and a pick is `PATCH /api/tracks/{id} {admin}`
// through the track patch mutation. The Track page shows only the Admin badge.

import { useCallback, useEffect, useRef } from 'react';

import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT } from '../../../../core/domain/track.ts';
import type { PlannerRole } from '../../features/chat/thread/model-pill.tsx';
import { useOperationFeedback, type OperationFeedbackState } from '../../ui/operation-feedback/public.tsx';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import { useState } from '../../ui/state/public.ts';

/** The disabled picker's reason where the server's `can_admin` is false. */
export const CANNOT_ADMIN_REASON = "This Track can't be an Area admin.";

export type PlannerRoleControl = Readonly<{
  value: PlannerRole;
  isDisabled: boolean;
  /** Set only when `can_admin` is false; a pick in flight disables it without a reason. */
  disabledReason: string | undefined;
  onChange: (role: PlannerRole) => void;
  /** A refused or unconfirmed pick, read through the track PATCH failure table. */
  feedback: OperationFeedbackState;
}>;

export function usePlannerRoleControl({ trackId, adminGrantedAt, canAdmin, setAdmin }: Readonly<{
  trackId: string;
  /** The server's `admin_granted_at`. */
  adminGrantedAt: number | null;
  /** The server's `can_admin`. */
  canAdmin: boolean;
  setAdmin: (admin: boolean) => Promise<unknown>;
}>): PlannerRoleControl {
  const value: PlannerRole = adminGrantedAt === null ? 'member' : 'admin';
  /* What the server offers to change: nothing, or away from the current role. */
  const offered = canAdmin ? value : null;
  const feedback = useOperationFeedback();
  const [pending, setPending] = useState(false);
  const pendingFrom = useRef<Readonly<{ from: PlannerRole }> | null>(null);
  const release = useCallback((token: NonNullable<typeof pendingFrom.current>) => {
    if (pendingFrom.current !== token) return;
    pendingFrom.current = null;
    setPending(false);
  }, [setPending]);
  useEffect(() => {
    // The Close and Reopen fence: held after a successful PATCH until the refetched detail offers something else.
    const token = pendingFrom.current;
    if (token === null || token.from === offered) return;
    release(token);
  }, [offered, release]);
  const onChange = useCommittedCallback(trackId, (role: PlannerRole) => {
    if (offered === null || role === offered || pendingFrom.current !== null) return;
    const token = { from: offered };
    pendingFrom.current = token;
    setPending(true);
    const admin = role === 'admin';
    void feedback.run(
      Promise.resolve().then(() => setAdmin(admin)),
      writeFailureText(TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT[admin ? 'grantAdmin' : 'revokeAdmin']),
    ).then((done) => {
      if (done) return;
      release(token);
    });
  });
  return {
    value,
    isDisabled: !canAdmin || pending,
    disabledReason: canAdmin ? undefined : CANNOT_ADMIN_REASON,
    onChange,
    feedback,
  };
}
