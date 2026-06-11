import { useCallback, useEffect, useMemo, useRef } from 'react';
import { listHarnessItems } from '../api/calm';
import { sharedEventStream } from '../api/events';
import type { HarnessItem } from '../api/generated-events';
import { useState } from '../shared/state';
import { parseHarnessItem, type ChatEntry } from './specChatItems';

const PAGE_LIMIT = 300;

export type VisibleChatEntry = ChatEntry & {
  queued?: boolean;
};

export interface SpecChatHistorySnapshot {
  entries: VisibleChatEntry[];
  hasEarlier: boolean;
  loadEarlierPending: boolean;
  loadEarlier(): Promise<void>;
  addEcho(text: string): void;
}

function isCompletedMessageItem(
  method: string,
  itemType: string | null,
): boolean {
  return (
    method === 'item/completed' &&
    (itemType === 'userMessage' || itemType === 'agentMessage')
  );
}

function parseRows(rows: HarnessItem[]): ChatEntry[] {
  return rows.flatMap((row) => {
    const parsed = parseHarnessItem(row);
    return parsed ? [parsed] : [];
  });
}

function appendUnique(
  current: ChatEntry[],
  incoming: ChatEntry[],
): ChatEntry[] {
  if (incoming.length === 0) return current;
  const seen = new Set(current.map((entry) => entry.id));
  const next = incoming.filter((entry) => !seen.has(entry.id));
  return next.length > 0 ? [...current, ...next] : current;
}

function prependUnique(
  current: ChatEntry[],
  incoming: ChatEntry[],
): ChatEntry[] {
  if (incoming.length === 0) return current;
  const seen = new Set(current.map((entry) => entry.id));
  const next = incoming.filter((entry) => !seen.has(entry.id));
  return next.length > 0 ? [...next, ...current] : current;
}

function normalizedText(text: string): string {
  return text.trim();
}

export function useSpecChatHistory(
  cardId: string | undefined,
): SpecChatHistorySnapshot {
  const cardIdRef = useRef<string | undefined>(cardId);
  cardIdRef.current = cardId;

  const oldestRawIdRef = useRef<number | null>(null);
  const newestRawIdRef = useRef<number | null>(null);
  const requestSeqRef = useRef(0);
  const echoIdRef = useRef(0);
  const loadingEarlierRef = useRef(false);
  const tailInFlightRef = useRef(false);
  const tailRefetchQueuedRef = useRef(false);

  const [entries, setEntries] = useState<ChatEntry[]>([]);
  const [echoes, setEchoes] = useState<VisibleChatEntry[]>([]);
  const [hasEarlier, setHasEarlier] = useState(false);
  const [loadEarlierPending, setLoadEarlierPending] = useState(false);

  const dropEchoesFor = useCallback((parsedEntries: ChatEntry[]) => {
    const userTexts = new Set(
      parsedEntries
        .filter((entry) => entry.kind === 'user')
        .map((entry) => normalizedText(entry.text))
        .filter(Boolean),
    );
    if (userTexts.size === 0) return;
    setEchoes((current) =>
      current.filter((echo) => !userTexts.has(normalizedText(echo.text))),
    );
  }, []);

  const replaceWithRows = useCallback((rows: HarnessItem[]) => {
    oldestRawIdRef.current = rows.length > 0 ? rows[0].id : null;
    newestRawIdRef.current =
      rows.length > 0 ? rows[rows.length - 1].id : null;
    setHasEarlier(rows.length === PAGE_LIMIT);

    const parsed = parseRows(rows);
    setEntries(parsed);
    dropEchoesFor(parsed);
  }, [dropEchoesFor]);

  const clearHistory = useCallback(() => {
    oldestRawIdRef.current = null;
    newestRawIdRef.current = null;
    tailInFlightRef.current = false;
    tailRefetchQueuedRef.current = false;
    loadingEarlierRef.current = false;
    setEntries([]);
    setEchoes([]);
    setHasEarlier(false);
    setLoadEarlierPending(false);
  }, []);

  const refetchLatest = useCallback(async (expectedCardId: string) => {
    const seq = requestSeqRef.current + 1;
    requestSeqRef.current = seq;

    try {
      const rows = await listHarnessItems(expectedCardId, {
        afterId: 0,
        limit: PAGE_LIMIT,
        direction: 'desc',
      });
      if (
        cardIdRef.current !== expectedCardId ||
        requestSeqRef.current !== seq
      ) {
        return;
      }
      replaceWithRows(rows);
    } catch {
      if (
        cardIdRef.current === expectedCardId &&
        requestSeqRef.current === seq
      ) {
        clearHistory();
      }
    }
  }, [clearHistory, replaceWithRows]);

  useEffect(() => {
    requestSeqRef.current += 1;
    clearHistory();
    if (cardId) {
      void refetchLatest(cardId);
    }
  }, [cardId, clearHistory, refetchLatest]);

  const loadEarlier = useCallback(async () => {
    const currentCardId = cardIdRef.current;
    const oldestRawId = oldestRawIdRef.current;
    if (!currentCardId || oldestRawId == null || loadingEarlierRef.current) {
      return;
    }

    loadingEarlierRef.current = true;
    setLoadEarlierPending(true);
    const seq = requestSeqRef.current;

    try {
      const rows = await listHarnessItems(currentCardId, {
        afterId: oldestRawId,
        limit: PAGE_LIMIT,
        direction: 'desc',
      });
      if (
        cardIdRef.current !== currentCardId ||
        requestSeqRef.current !== seq
      ) {
        return;
      }

      if (rows.length === 0) {
        setHasEarlier(false);
        return;
      }

      oldestRawIdRef.current = rows[0].id;
      if (newestRawIdRef.current == null) {
        newestRawIdRef.current = rows[rows.length - 1].id;
      }
      setHasEarlier(rows.length === PAGE_LIMIT);

      const parsed = parseRows(rows);
      setEntries((current) => prependUnique(current, parsed));
      dropEchoesFor(parsed);
    } finally {
      loadingEarlierRef.current = false;
      setLoadEarlierPending(false);
    }
  }, [dropEchoesFor]);

  const fetchTail = useCallback(async () => {
    if (tailInFlightRef.current) {
      tailRefetchQueuedRef.current = true;
      return;
    }

    tailInFlightRef.current = true;
    try {
      do {
        tailRefetchQueuedRef.current = false;
        const currentCardId = cardIdRef.current;
        if (!currentCardId) return;

        const newestRawId = newestRawIdRef.current;
        if (newestRawId == null) {
          await refetchLatest(currentCardId);
          return;
        }

        const seq = requestSeqRef.current;
        const rows = await listHarnessItems(currentCardId, {
          afterId: newestRawId,
          limit: PAGE_LIMIT,
          direction: 'asc',
        });
        if (
          cardIdRef.current !== currentCardId ||
          requestSeqRef.current !== seq
        ) {
          return;
        }

        if (rows.length > 0) {
          if (oldestRawIdRef.current == null) {
            oldestRawIdRef.current = rows[0].id;
          }
          newestRawIdRef.current = rows[rows.length - 1].id;

          const parsed = parseRows(rows);
          setEntries((current) => appendUnique(current, parsed));
          dropEchoesFor(parsed);
        }
      } while (tailRefetchQueuedRef.current);
    } finally {
      tailInFlightRef.current = false;
    }
  }, [dropEchoesFor, refetchLatest]);

  useEffect(() => {
    if (!cardId) return;

    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const off = stream.on((ev) => {
      if (
        ev.ev === 'harness.transcript.cleared' &&
        ev.data.card_id === cardId
      ) {
        requestSeqRef.current += 1;
        clearHistory();
        void refetchLatest(cardId);
        return;
      }

      if (
        ev.ev === 'harness.item.added' &&
        ev.data.card_id === cardId &&
        isCompletedMessageItem(ev.data.method, ev.data.item_type)
      ) {
        void fetchTail();
      }
    });

    return () => {
      off();
    };
  }, [cardId, clearHistory, fetchTail, refetchLatest]);

  const addEcho = useCallback((text: string) => {
    const trimmed = text.trim();
    if (!trimmed) return;

    echoIdRef.current -= 1;
    setEchoes((current) => [
      ...current,
      {
        id: echoIdRef.current,
        kind: 'user',
        text: trimmed,
        atMs: Date.now(),
        queued: true,
      },
    ]);
  }, []);

  const visibleEntries = useMemo<VisibleChatEntry[]>(
    () => [...entries, ...echoes],
    [entries, echoes],
  );

  return {
    entries: visibleEntries,
    hasEarlier,
    loadEarlierPending,
    loadEarlier,
    addEcho,
  };
}
