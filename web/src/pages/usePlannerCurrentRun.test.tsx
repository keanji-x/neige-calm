// Issue #668 — `usePlannerCurrentRun` contract tests.
//
// Production publishes NO status overlay for planner cards (the overlay mock
// answers null, exactly like a live stack), so `working` / `stopping` /
// `fsm` must derive from the real wire signals: the `GET /planner/run` seed
// and `harness.phase.changed` events carrying snake_case
// `HarnessPhaseTag` values.
//
// Also covers error-state hygiene: a stop against a dormant harness 409s
// and sets `stopError` steering the user to Reset; a successful reset
// mints a fresh session, so the stale stop error (like the stale submit
// error / dormant flag) must be cleared.

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const apiMocks = vi.hoisted(() => ({
  getPlannerRun: vi.fn(),
  interruptPlannerCard: vi.fn(),
  resetPlannerCard: vi.fn(),
  sendPlannerInput: vi.fn(),
}));

const streamMocks = vi.hoisted(() => {
  const listeners = new Set<(ev: unknown) => void>();
  return {
    listeners,
    stream: {
      addTopic: vi.fn(),
      on: vi.fn((fn: (ev: unknown) => void) => {
        listeners.add(fn);
        return () => {
          listeners.delete(fn);
        };
      }),
    },
  };
});

vi.mock('../api/calm', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/calm')>();
  return {
    ...actual,
    getPlannerRun: apiMocks.getPlannerRun,
    interruptPlannerCard: apiMocks.interruptPlannerCard,
    resetPlannerCard: apiMocks.resetPlannerCard,
    sendPlannerInput: apiMocks.sendPlannerInput,
  };
});

vi.mock('../api/events', () => ({
  sharedEventStream: vi.fn(() => streamMocks.stream),
}));

// Production-accurate: no status overlay is ever published for planner cards.
vi.mock('../cards/overlayRegistry', () => ({
  useCardStatusOverlay: vi.fn(() => null),
}));

import { CalmApiError } from '../api/calm';
import { usePlannerCurrentRun } from './usePlannerCurrentRun';

const dormantError = () =>
  new CalmApiError(
    409,
    'planner_harness_dormant',
    'no live planner harness session for card card_1; reset to start a session',
  );

/** Emit a `harness.phase.changed` event in its real wire shape. */
async function emitPhase(
  newPhase: string,
  { cardId = 'card_1', oldPhase = 'idle' } = {},
) {
  await act(async () => {
    for (const listener of streamMocks.listeners) {
      listener({
        ev: 'harness.phase.changed',
        data: {
          worker_session_id: 'runtime_1',
          card_id: cardId,
          track_id: 'track_1',
          old_phase: oldPhase,
          new_phase: newPhase,
        },
      });
    }
  });
}

function plannerRun(phase: string | null) {
  return {
    card_id: 'card_1',
    worker_session_id: phase == null ? null : 'runtime_1',
    phase,
  };
}

async function renderRun(cardId = 'card_1') {
  const view = renderHook(() => usePlannerCurrentRun(cardId));
  // Flush the `GET /planner/run` seed fetch.
  await act(async () => {});
  return view;
}

describe('usePlannerCurrentRun', () => {
  beforeEach(() => {
    apiMocks.getPlannerRun.mockReset();
    apiMocks.getPlannerRun.mockResolvedValue(plannerRun(null));
    apiMocks.interruptPlannerCard.mockReset();
    apiMocks.resetPlannerCard.mockReset();
    apiMocks.sendPlannerInput.mockReset();
    streamMocks.listeners.clear();
    streamMocks.stream.addTopic.mockClear();
    streamMocks.stream.on.mockClear();
  });

  it('keeps every gate closed while no phase signal exists (dormant seed)', async () => {
    const { result } = await renderRun();

    expect(apiMocks.getPlannerRun).toHaveBeenCalledWith('card_1');
    expect(result.current.phase).toBeNull();
    expect(result.current.working).toBe(false);
    expect(result.current.stopping).toBe(false);
    expect(result.current.fsm).toBe('Starting');
  });

  it('opens on harness.phase.changed turn_running and closes on turn_completed', async () => {
    const { result } = await renderRun();

    await emitPhase('turn_running', { oldPhase: 'issuing_turn' });
    expect(result.current.phase).toBe('turn_running');
    expect(result.current.working).toBe(true);
    expect(result.current.stopping).toBe(false);
    expect(result.current.fsm).toBe('Working');

    await emitPhase('turn_completed', { oldPhase: 'turn_running' });
    expect(result.current.working).toBe(false);
    expect(result.current.fsm).toBe('Idle');
  });

  it('counts issuing_turn as working and issuing_interrupt as stopping', async () => {
    const { result } = await renderRun();

    await emitPhase('issuing_turn');
    expect(result.current.working).toBe(true);
    expect(result.current.stopping).toBe(false);

    await emitPhase('issuing_interrupt', { oldPhase: 'turn_running' });
    expect(result.current.working).toBe(false);
    expect(result.current.stopping).toBe(true);
    // The chip reuses the Working color while an interrupt is in flight.
    expect(result.current.fsm).toBe('Working');
  });

  it('ignores phase events for other cards', async () => {
    const { result } = await renderRun();

    await emitPhase('turn_running', { cardId: 'card_other' });
    expect(result.current.phase).toBeNull();
    expect(result.current.working).toBe(false);
  });

  it('seeds the phase from GET /planner/run when no event has arrived', async () => {
    apiMocks.getPlannerRun.mockResolvedValue(plannerRun('turn_running'));

    const { result } = await renderRun();

    await waitFor(() => {
      expect(result.current.working).toBe(true);
    });
    expect(result.current.phase).toBe('turn_running');
    expect(result.current.fsm).toBe('Working');
  });

  it('lets an event that lands before the seed resolves win', async () => {
    let resolveSeed: (run: unknown) => void = () => {};
    apiMocks.getPlannerRun.mockReturnValue(
      new Promise((res) => {
        resolveSeed = res;
      }),
    );

    const { result } = await renderRun();
    await emitPhase('turn_running', { oldPhase: 'issuing_turn' });

    await act(async () => {
      resolveSeed(plannerRun('idle'));
    });
    // The event is strictly newer than the snapshot read — it must not be
    // clobbered by the slower seed.
    expect(result.current.phase).toBe('turn_running');
    expect(result.current.working).toBe(true);
  });

  it('clears the phase (closing the gates) on successful reset', async () => {
    apiMocks.resetPlannerCard.mockResolvedValue(undefined);
    const { result } = await renderRun();

    await emitPhase('turn_running');
    expect(result.current.working).toBe(true);

    await act(async () => {
      await result.current.reset();
    });
    expect(result.current.phase).toBeNull();
    expect(result.current.working).toBe(false);
  });

  it('clears a dormant stop error on successful reset', async () => {
    apiMocks.interruptPlannerCard.mockRejectedValue(dormantError());
    apiMocks.resetPlannerCard.mockResolvedValue(undefined);

    const { result } = await renderRun();

    await act(async () => {
      await expect(result.current.stop()).rejects.toThrow();
    });
    expect(result.current.stopError).toMatch(/Reset to start a session/);

    await act(async () => {
      await result.current.reset();
    });
    expect(result.current.stopError).toBeNull();
  });

  it('clears the dormant submit error and flag on successful reset', async () => {
    apiMocks.sendPlannerInput.mockRejectedValue(dormantError());
    apiMocks.resetPlannerCard.mockResolvedValue(undefined);

    const { result } = await renderRun();

    await act(async () => {
      await expect(result.current.submit('hello')).rejects.toThrow();
    });
    expect(result.current.submitDormant).toBe(true);
    expect(result.current.submitError).toMatch(/Reset to start a session/);

    await act(async () => {
      await result.current.reset();
    });
    expect(result.current.submitDormant).toBe(false);
    expect(result.current.submitError).toBeNull();
  });

  it('keeps the stop error when reset itself fails', async () => {
    apiMocks.interruptPlannerCard.mockRejectedValue(dormantError());
    apiMocks.resetPlannerCard.mockRejectedValue(new Error('reset exploded'));

    const { result } = await renderRun();

    await act(async () => {
      await expect(result.current.stop()).rejects.toThrow();
    });
    await act(async () => {
      await expect(result.current.reset()).rejects.toThrow('reset exploded');
    });
    expect(result.current.stopError).toMatch(/Reset to start a session/);
    expect(result.current.resetError).toBe('reset exploded');
  });
});
