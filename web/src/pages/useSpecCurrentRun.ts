import { useCallback, useEffect } from 'react';
import { CalmApiError, resetSpecCard, sendSpecInput } from '../api/calm';
import { sharedEventStream } from '../api/events';
import { useCardStatusOverlay } from '../cards/overlayRegistry';
import { useState } from '../shared/state';
import type { FsmState } from '../types';

export type { FsmState };

export interface LatestHarnessActivity {
  /** Most recent tool/function name being called, or null when idle. */
  toolLabel: string | null;
  /** Brief status string for the pill ("running", "completed", etc.). */
  toolStatus: string | null;
}

export interface SpecRunSnapshot {
  cardId: string | null;
  /** Raw status from overlay (e.g. "TurnRunning", "Idle"). */
  rawState: string;
  /** Normalized FSM state via toFsmState. */
  fsm: FsmState;
  /** Latest harness phase from harness.phase.changed event. */
  phase: string | null;
  /** Latest active tool/function call from harness items. */
  latestTool: LatestHarnessActivity;
  /** Reset session state. */
  resetPending: boolean;
  resetError: string | null;
  reset(): Promise<void>;
  /** Send a user follow-up to the spec harness. */
  submit(text: string): Promise<void>;
  submitPending: boolean;
  submitError: string | null;
  /**
   * True when the last submit failed with the server's typed
   * `spec_harness_dormant` 409 — no recoverable harness session exists
   * for this card and the user should Reset to start one (issue #649 i2).
   */
  submitDormant: boolean;
}

export function toFsmState(state: string | undefined): FsmState {
  switch (state) {
    case 'Starting':
    case 'Idle':
    case 'Working':
    case 'AwaitingInput':
    case 'Errored':
    case 'Done':
      return state;
    case 'starting':
      return 'Starting';
    case 'running':
      return 'Working';
    case 'idle':
      return 'Idle';
    case 'turn_pending':
      return 'AwaitingInput';
    case 'failed':
      return 'Errored';
    case 'exited':
    case 'superseded':
      return 'Done';
    default:
      return 'Starting';
  }
}

export function humanizeToken(token: string): string {
  return token
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error ? err.message : fallback;
}

export function useSpecCurrentRun(cardId: string | undefined): SpecRunSnapshot {
  const status = useCardStatusOverlay(cardId);
  const rawState = status?.state ?? 'Starting';
  const fsm = toFsmState(rawState);
  const [phase, setPhase] = useState<string | null>(null);
  const [resetPending, setResetPending] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  const [submitPending, setSubmitPending] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitDormant, setSubmitDormant] = useState(false);

  useEffect(() => {
    setPhase(null);
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const off = stream.on((ev) => {
      if (
        ev.ev === 'harness.phase.changed' &&
        ev.data.card_id === cardId
      ) {
        setPhase(ev.data.new_phase);
      }
    });
    return () => {
      off();
    };
  }, [cardId]);

  useEffect(() => {
    setResetPending(false);
    setResetError(null);
    setSubmitPending(false);
    setSubmitError(null);
    setSubmitDormant(false);
  }, [cardId]);

  const reset = useCallback(async () => {
    if (!cardId) {
      const err = new Error('Spec card unavailable');
      setResetError(err.message);
      throw err;
    }
    setResetPending(true);
    setResetError(null);
    try {
      await resetSpecCard(cardId);
      setPhase(null);
      // A successful reset mints a fresh harness session — the dormant
      // state (and its stale error) no longer applies.
      setSubmitDormant(false);
      setSubmitError(null);
    } catch (err) {
      const msg = errorMessage(err, 'Reset failed');
      setResetError(msg);
      throw err;
    } finally {
      setResetPending(false);
    }
  }, [cardId]);

  const submit = useCallback(async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed) {
      const err = new Error('Message is required');
      setSubmitError(err.message);
      throw err;
    }
    if (!cardId) {
      const err = new Error('Spec card unavailable');
      setSubmitError(err.message);
      throw err;
    }
    setSubmitPending(true);
    setSubmitError(null);
    setSubmitDormant(false);
    try {
      await sendSpecInput(cardId, trimmed);
    } catch (err) {
      if (err instanceof CalmApiError && err.code === 'spec_harness_dormant') {
        setSubmitDormant(true);
        setSubmitError(
          "Spec Agent isn't running for this wave — Reset to start a session",
        );
      } else {
        setSubmitError(errorMessage(err, 'Failed to send message'));
      }
      throw err;
    } finally {
      setSubmitPending(false);
    }
  }, [cardId]);

  return {
    cardId: cardId ?? null,
    rawState,
    fsm,
    phase,
    latestTool: { toolLabel: null, toolStatus: null },
    resetPending,
    resetError,
    reset,
    submit,
    submitPending,
    submitError,
    submitDormant,
  };
}
