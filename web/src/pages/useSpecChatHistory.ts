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
   * stop would be visually silent). Anchored to the newest server entry at
   * creation time so later-arriving rows render below it, not above it.
   * Shares the echo lifecycle: cleared on card change and transcript-clear,
   * never persisted server-side.
   */
  addSystemNote(text: string): void;
}

/**
 * FE-local system note (#668), anchored in transcript order.
 * `afterEntryId` is the id of the newest server entry when the note was
 * created (null when the transcript was empty), so the note stays between
 * the entries it was created between even after newer rows arrive.
 * A note created while the INITIAL history fetch is still in flight cannot
 * know its anchor yet — it carries `'pending'` and is resolved to the
 * newest entry of the initial page when that page lands (null only if the
 * transcript is genuinely empty), so it renders at the end of the existing
 * conversation rather than at the top (#668 review P3).
 */
type SystemNote = {
  id: number;
  text: string;
  atMs: number;
  afterEntryId: number | null | 'pending';
};

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
  // False until the first full-page fetch for the current card resolves;
  // notes created before that get a 'pending' anchor (#668 review P3).
  const initialLoadDoneRef = useRef(false);

  const [entries, setEntries] = useState<ChatEntry[]>([]);
  const [echoes, setEchoes] = useState<VisibleChatEntry[]>([]);
  const [systemNotes, setSystemNotes] = useState<SystemNote[]>([]);
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

    // The initial page has landed: resolve 'pending' note anchors to the
    // newest loaded entry so a stop-before-load note renders at the end of
    // the conversation, not the top (#668 review P3). Null only when the
    // transcript is genuinely empty.
    initialLoadDoneRef.current = true;
    const newestEntryId = parsed.length > 0 ? parsed[parsed.length - 1].id : null;
    setSystemNotes((current) =>
      current.some((note) => note.afterEntryId === 'pending')
        ? current.map((note) =>
            note.afterEntryId === 'pending'
              ? { ...note, afterEntryId: newestEntryId }
              : note,
          )
        : current,
    );

    dropEchoesFor(parsed);
  }, [dropEchoesFor]);

  const clearHistory = useCallback(() => {
    oldestRawIdRef.current = null;
    newestRawIdRef.current = null;
    initialLoadDoneRef.current = false;
    tailInFlightRef.current = false;
    tailRefetchQueuedRef.current = false;
    loadingEarlierRef.current = false;
    entriesRef.current = [];
    setEntries([]);
    setEchoes([]);
    setSystemNotes([]);
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
    // Before the initial history page resolves the anchor is unknowable —
    // an empty entriesRef just means "not loaded yet". Mark it 'pending';
    // replaceWithRows resolves it when the page lands (#668 review P3).
    const loaded = entriesRef.current;
    const afterEntryId: number | null | 'pending' =
      !initialLoadDoneRef.current
        ? 'pending'
        : loaded.length > 0
          ? loaded[loaded.length - 1].id
          : null;
    echoIdRef.current -= 1;
    setSystemNotes((current) => [
      ...current,
      {
        id: echoIdRef.current,
        text: trimmed,
        atMs: Date.now(),
        afterEntryId,
      },
    ]);
  }, []);

  const visibleEntries = useMemo<VisibleChatEntry[]>(() => {
    if (systemNotes.length === 0) return [...entries, ...echoes];

    // Insert each note right after its anchor entry so later-arriving rows
    // render below it (#668 review P2). Slot k means "after entries[k-1]";
    // slot 0 renders before all loaded entries — used for null anchors,
    // not-yet-resolved 'pending' anchors (the transcript is still empty
    // then, so slot 0 is the whole list), and anchors older than the loaded
    // window (paged out). An anchor id that is in range but no longer
    // loaded falls back to id order: after the last loaded entry whose
    // id <= anchor.
    const slots = new Map<number, VisibleChatEntry[]>();
    for (const note of systemNotes) {
      let slot = 0;
      if (typeof note.afterEntryId === 'number') {
        for (let i = entries.length - 1; i >= 0; i -= 1) {
          if (entries[i].id <= note.afterEntryId) {
            slot = i + 1;
            break;
          }
        }
      }
      const row: VisibleChatEntry = {
        id: note.id,
        kind: 'system',
        text: note.text,
        atMs: note.atMs,
      };
      const bucket = slots.get(slot);
      if (bucket) {
        bucket.push(row);
      } else {
        slots.set(slot, [row]);
      }
    }

    const merged: VisibleChatEntry[] = [...(slots.get(0) ?? [])];
    entries.forEach((entry, index) => {
      merged.push(entry);
      const bucket = slots.get(index + 1);
      if (bucket) merged.push(...bucket);
    });
    // Echo bubbles keep their end-of-list behavior.
    return [...merged, ...echoes];
  }, [entries, echoes, systemNotes]);

  return {
    entries: visibleEntries,
    hasEarlier,
    loadEarlierPending,
    loadEarlier,
    addEcho,
    addSystemNote,
  };
}
