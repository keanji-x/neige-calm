import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const streamMock = vi.hoisted(() => {
  const listeners = new Set<(ev: unknown) => void>();
  return {
    addTopic: vi.fn(),
    on: vi.fn((listener: (ev: unknown) => void) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    }),
    emit(ev: unknown) {
      for (const listener of Array.from(listeners)) listener(ev);
    },
    reset() {
      listeners.clear();
      this.addTopic.mockClear();
      this.on.mockClear();
    },
  };
});

vi.mock('../api/events', () => ({
  sharedEventStream: vi.fn(() => streamMock),
}));

import { useCardOverlay } from './useCardOverlay';

describe('useCardOverlay', () => {
  beforeEach(() => {
    streamMock.reset();
  });

  it('sets and clears matching card overlay payloads', () => {
    const { result } = renderHook(() =>
      useCardOverlay<{ state: string }>('card_1', 'status'),
    );

    expect(streamMock.addTopic).toHaveBeenCalledWith('card:card_1');
    expect(result.current).toBeNull();

    act(() => {
      streamMock.emit({
        ev: 'overlay.set',
        data: {
          entity_kind: 'card',
          entity_id: 'card_1',
          kind: 'status',
          payload: { state: 'Working' },
        },
      });
    });
    expect(result.current).toEqual({ state: 'Working' });

    act(() => {
      streamMock.emit({
        ev: 'overlay.set',
        data: {
          entity_kind: 'card',
          entity_id: 'card_1',
          kind: 'other',
          payload: { state: 'Ignored' },
        },
      });
    });
    expect(result.current).toEqual({ state: 'Working' });

    act(() => {
      streamMock.emit({
        ev: 'overlay.deleted',
        data: {
          entity_kind: 'card',
          entity_id: 'card_1',
          kind: 'status',
        },
      });
    });
    expect(result.current).toBeNull();
  });

  it('does not subscribe without a cardId', () => {
    const { result } = renderHook(() =>
      useCardOverlay<{ state: string }>(undefined, 'status'),
    );

    expect(result.current).toBeNull();
    expect(streamMock.addTopic).not.toHaveBeenCalled();
    expect(streamMock.on).not.toHaveBeenCalled();
  });
});
