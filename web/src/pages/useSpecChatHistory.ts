import { useCallback, useEffect, useMemo, useRef } from 'react';
import { listHarnessItems } from '../api/calm';
import { sharedEventStream } from '../api/events';
import type { HarnessItem } from '../api/generated-events';
import { useState } from '../shared/state';
import { parseHarnessItem, type ChatEntry } from './specChatItems';

const PAGE_LIMIT = 300;
const ECHO_RECONCILE_LOOKBACK = 5;

export type VisibleChatEntry = ChatEntry & {
  queued?: boolean;
};

export interface SpecChatHistorySnapshot {
  entries: VisibleChatEntry[];
  hasEarlier: boolean;
  loadEarlierPending: boolean;
  loadEarlier(): Promise<void>;
  addEcho(text: string): void;
  /**
   * Append a FE-local system row (#668 — e.g. "Turn stopped" after a user
   * stop; interrupted turns never emit `item/completed`, so without it the
   * stop would be visually silent). Shares the echo lifecycle: cleared on
   * card change and transcript-clear, never persisted server-side.
   */
  addSystemNote(text: string): void;
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

function userTextMatchesEcho(userText: string, echoText: string): boolean {
  const normalizedUserText = normalizedText(userText);
  const normalizedEchoText = normalizedText(echoText);
  return (
    normalizedUserText.length > 0 &&
    normalizedEchoText.length > 0 &&
    (normalizedUserText === normalizedEchoText ||
      normalizedUserText.startsWith(`${normalizedEchoText}\n`))
  );
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
  const entriesRef = useRef<ChatEntry[]>([]);

  const dropEchoesFor = useCallback((parsedEntries: ChatEntry[]) => {
    const userTexts = parsedEntries
      .filter((entry) => entry.kind === 'user')
      .map((entry) => normalizedText(entry.text))
      .filter(Boolean);
    if (userTexts.length === 0) return;
    setEchoes((current) => {
      const matchedUserIndexes = new Set<number>();
      const next = current.filter((echo) => {
        // System notes (#668) live in the same FE-local list for lifecycle
        // (clear on card change / transcript-clear) but never reconcile
        // against landed user rows.
        if (echo.kind !== 'user') return true;
        const matchedIndex = userTexts.findIndex(
          (userText, index) =>
            !matchedUserIndexes.has(index) &&
            userTextMatchesEcho(userText, echo.text),
        );
        if (matchedIndex < 0) return true;
        matchedUserIndexes.add(matchedIndex);
        return false;
      });
      return next.length === current.length ? current : next;
    });
  }, []);

  const replaceWithRows = useCallback((rows: HarnessItem[]) => {
    oldestRawIdRef.current = rows.length > 0 ? rows[0].id : null;
    newestRawIdRef.current =
      rows.length > 0 ? rows[rows.length - 1].id : null;
    setHasEarlier(rows.length === PAGE_LIMIT);

    const parsed = parseRows(rows);
    entriesRef.current = parsed;
    setEntries(parsed);
    dropEchoesFor(parsed);
  }, [dropEchoesFor]);

  const clearHistory = useCallback(() => {
    oldestRawIdRef.current = null;
    newestRawIdRef.current = null;
    tailInFlightRef.current = false;
    tailRefetchQueuedRef.current = false;
    loadingEarlierRef.current = false;
    entriesRef.current = [];
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
      setEntries((current) => {
        const next = prependUnique(current, parsed);
        entriesRef.current = next;
        return next;
      });
      dropEchoesFor(parsed);
    } catch {
      // Keep the visible window; older-page failures are retriable.
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
          continue;
        }

        const seq = requestSeqRef.current;
        let rows: HarnessItem[] = [];
        do {
          const afterId = newestRawIdRef.current;
          if (afterId == null) break;

          rows = await listHarnessItems(currentCardId, {
            afterId,
            limit: PAGE_LIMIT,
            direction: 'asc',
          });
          if (
            cardIdRef.current !== currentCardId ||
            requestSeqRef.current !== seq
          ) {
            return;
          }
          if (rows.length === 0) break;

          if (oldestRawIdRef.current == null) {
            oldestRawIdRef.current = rows[0].id;
          }
          newestRawIdRef.current = rows[rows.length - 1].id;

          const parsed = parseRows(rows);
          setEntries((current) => {
            const next = appendUnique(current, parsed);
            entriesRef.current = next;
            return next;
          });
          dropEchoesFor(parsed);
        } while (rows.length === PAGE_LIMIT);
      } while (tailRefetchQueuedRef.current);
    } catch {
      // Network/5xx mid tail-fetch: keep the visible window and stay
      // retriable, mirroring the initial-load / loadEarlier paths. The
      // listener fires this as `void fetchTail()`, so rethrowing would be
      // an unhandled rejection. Drop any queued refetch; the next
      // harness.item.added event retries cleanly.
      tailRefetchQueuedRef.current = false;
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
    const recentEntries = entriesRef.current.slice(-ECHO_RECONCILE_LOOKBACK);
    if (
      recentEntries.some(
        (entry) =>
          entry.kind === 'user' && userTextMatchesEcho(entry.text, trimmed),
      )
    ) {
      return;
    }

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

  const addSystemNote = useCallback((text: string) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    echoIdRef.current -= 1;
    setEchoes((current) => [
      ...current,
      {
        id: echoIdRef.current,
        kind: 'system',
        text: trimmed,
        atMs: Date.now(),
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
    addSystemNote,
  };
}
