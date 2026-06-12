// Issue #668 review fix — `useSpecCurrentRun` error-state hygiene.
//
// A stop against a dormant harness 409s and sets `stopError` steering the
// user to Reset; a successful reset mints a fresh session, so the stale
// stop error (like the stale submit error / dormant flag) must be cleared.

import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const apiMocks = vi.hoisted(() => ({
  interruptSpecCard: vi.fn(),
  resetSpecCard: vi.fn(),
  sendSpecInput: vi.fn(),
}));

vi.mock('../api/calm', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/calm')>();
  return {
    ...actual,
    interruptSpecCard: apiMocks.interruptSpecCard,
    resetSpecCard: apiMocks.resetSpecCard,
    sendSpecInput: apiMocks.sendSpecInput,
  };
});

vi.mock('../api/events', () => ({
  sharedEventStream: vi.fn(() => ({
    addTopic: vi.fn(),
    on: vi.fn(() => () => {}),
  })),
}));

vi.mock('../cards/overlayRegistry', () => ({
  useCardStatusOverlay: vi.fn(() => null),
}));

import { CalmApiError } from '../api/calm';
import { useSpecCurrentRun } from './useSpecCurrentRun';

const dormantError = () =>
  new CalmApiError(
    409,
    'spec_harness_dormant',
    'no live spec harness session for card card_1; reset to start a session',
  );

describe('useSpecCurrentRun', () => {
  beforeEach(() => {
    apiMocks.interruptSpecCard.mockReset();
    apiMocks.resetSpecCard.mockReset();
    apiMocks.sendSpecInput.mockReset();
  });

  it('clears a dormant stop error on successful reset', async () => {
    apiMocks.interruptSpecCard.mockRejectedValue(dormantError());
    apiMocks.resetSpecCard.mockResolvedValue(undefined);

    const { result } = renderHook(() => useSpecCurrentRun('card_1'));

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
    apiMocks.sendSpecInput.mockRejectedValue(dormantError());
    apiMocks.resetSpecCard.mockResolvedValue(undefined);

    const { result } = renderHook(() => useSpecCurrentRun('card_1'));

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
    apiMocks.interruptSpecCard.mockRejectedValue(dormantError());
    apiMocks.resetSpecCard.mockRejectedValue(new Error('reset exploded'));

    const { result } = renderHook(() => useSpecCurrentRun('card_1'));

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
