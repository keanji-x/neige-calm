import { useEffect } from 'react';
import { sharedEventStream } from '../api/events';
import { useState } from '../shared/state';

export function useCardOverlay<T>(
  cardId: string | undefined,
  overlayKind: string,
): T | null {
  const [payload, setPayload] = useState<T | null>(null);

  useEffect(() => {
    if (!cardId) {
      setPayload(null);
      return;
    }
    setPayload(null);
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const off = stream.on((ev) => {
      if (ev.ev === 'overlay.set') {
        const o = ev.data;
        if (
          o.entity_kind === 'card' &&
          o.entity_id === cardId &&
          o.kind === overlayKind
        ) {
          setPayload(o.payload as T);
        }
        return;
      }
      if (ev.ev === 'overlay.deleted') {
        const o = ev.data;
        if (
          o.entity_kind === 'card' &&
          o.entity_id === cardId &&
          o.kind === overlayKind
        ) {
          setPayload(null);
        }
      }
    });
    return () => {
      off();
    };
  }, [cardId, overlayKind]);

  return payload;
}

export interface CardStatusPayload {
  state: string;
}

export function useCardStatusOverlay(
  cardId: string | undefined,
): CardStatusPayload | null {
  return useCardOverlay<CardStatusPayload>(cardId, 'status');
}
