import { useCallback, useEffect } from 'react';
import {
  CalmApiError,
  getSpecRun,
  interruptSpecCard,
  resetSpecCard,
  sendSpecInput,
} from '../api/calm';
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
  /**
   * Normalized FSM state. No status overlay is ever published for spec
   * cards in production (#668 fix), so when a harness phase is known it
   * wins; the overlay-derived state is only the fallback.
   */
  fsm: FsmState;
  /**
   * Latest harness phase: seeded from `GET /spec/run` on mount/card
   * change, then updated from `harness.phase.changed` events. Null until
   * either source answers (or when the harness is dormant).
   */
  phase: string | null;
  /**
   * True while a turn is live (`issuing_turn` / `turn_running`) — the
   * gate for every stop affordance and the typing indicator (#668 fix:
   * `fsm === 'Working'` never fired because spec cards have no overlay).
   */
  working: boolean;
  /** True while an interrupt is in flight (`issuing_interrupt`). */
  stopping: boolean;
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
  /**
   * Stop the running turn (#668). Resolves with the server's `stopped`
   * flag: `true` when an interrupt was dispatched at a running turn,
   * `false` when the harness was already idle (graceful no-op).
   */
  stop(): Promise<boolean>;
  stopPending: boolean;
  stopError: string | null;
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

/**
 * Map a `HarnessPhaseTag` wire value onto the chip's FSM styling buckets.
 * `issuing_interrupt` deliberately reuses the Working color — an interrupt
 * in flight is still "the agent is busy" to the reader.
 */
export function phaseToFsm(phase: string): FsmState {
  switch (phase) {
    case 'pending_thread_start':
      return 'Starting';
    case 'issuing_turn':
    case 'turn_running':
    case 'issuing_interrupt':
      return 'Working';
    case 'idle':
    case 'turn_completed':
    case 'resumed':
      return 'Idle';
    case 'wedged':
      return 'Errored';
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
  const [phase, setPhase] = useState<string | null>(null);
  // The harness phase is the real live signal — no status overlay is ever
  // published for spec cards, so `toFsmState(rawState)` alone would pin
  // the chip on 'Starting' forever (#668 fix).
  const fsm = phase != null ? phaseToFsm(phase) : toFsmState(rawState);
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = phase === 'issuing_interrupt';
  const [resetPending, setResetPending] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  const [submitPending, setSubmitPending] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitDormant, setSubmitDormant] = useState(false);
  const [stopPending, setStopPending] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);

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
    // Seed the phase: `harness.phase.changed` only reports transitions, so
    // a page opened mid-turn would otherwise sit on null until the next
    // one (#668 fix). An event that lands first wins — it is strictly
    // newer than the snapshot read.
    let cancelled = false;
    getSpecRun(cardId).then(
      (run) => {
        if (cancelled || run.phase == null) return;
        setPhase((current) => current ?? run.phase);
      },
      () => {
        // Best-effort seed: on failure the gates simply stay closed until
        // the next phase transition arrives over the event stream.
      },
    );
    return () => {
      cancelled = true;
      off();
    };
  }, [cardId]);

  useEffect(() => {
    setResetPending(false);
    setResetError(null);
    setSubmitPending(false);
    setSubmitError(null);
    setSubmitDormant(false);
    setStopPending(false);
    setStopError(null);
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
      // state (and its stale errors) no longer applies.
      setSubmitDormant(false);
      setSubmitError(null);
      setStopError(null);
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

  const stop = useCallback(async () => {
    if (!cardId) {
      const err = new Error('Spec card unavailable');
      setStopError(err.message);
      throw err;
    }
    setStopPending(true);
    setStopError(null);
    try {
      const res = await interruptSpecCard(cardId);
      return res.stopped;
    } catch (err) {
      if (err instanceof CalmApiError && err.code === 'spec_harness_dormant') {
        setStopError(
          "Spec Agent isn't running for this wave — Reset to start a session",
        );
      } else {
        setStopError(errorMessage(err, 'Failed to stop turn'));
      }
      throw err;
    } finally {
      setStopPending(false);
    }
  }, [cardId]);

  return {
    cardId: cardId ?? null,
    rawState,
    fsm,
    phase,
    working,
    stopping,
    latestTool: { toolLabel: null, toolStatus: null },
    resetPending,
    resetError,
    reset,
    submit,
    submitPending,
    submitError,
    submitDormant,
    stop,
    stopPending,
    stopError,
  };
}
