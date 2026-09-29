// `@` in a composer whose message a Planner reads (#1881): Astryx's trigger menu over the
// area's tags, reports and blocks. The caller hands in the search (the area and track are
// composition facts); this file owns the menu row, the chip, and which answer may be shown.

import { useMemo, useRef } from 'react';
import type { ChatComposerToken, ChatComposerTrigger } from '@astryxdesign/core/Chat';
import type { SearchableItem, SearchSource } from '@astryxdesign/core/Typeahead';

import type { MentionKind, MentionSearch, MentionSuggestion } from '../../../../../core/domain/mentions.ts';
import styles from './mention-trigger.module.css';

/** The headings Astryx's `groupItems` draws, in the order the suggestions arrive. */
const GROUP_HEADING: Readonly<Record<MentionKind, string>> = Object.freeze({
  tag: 'Tags',
  track: 'Tracks',
  block: 'Blocks',
});

/**
 * How long a keystroke waits before its request goes out. The wait is this source's, not
 * Astryx's: `useTriggerMenu` calls `search('')` on every keystroke only to learn whether the
 * source is async, discards the answer, and calls `cancel()` right before the real search, so
 * a source that fetched at once would send a second, empty-query request per keystroke. A
 * composer carrying this trigger therefore runs `ChatComposerInput` with `debounceMs={0}`:
 * Astryx's own delay would let that probe's timer fire before the `cancel()` arrives.
 */
export const MENTION_SEARCH_DELAY_MS = 120;

type MentionItem = SearchableItem<Readonly<{ group: string; suggestion: MentionSuggestion }>>;

function itemOf(suggestion: MentionSuggestion): MentionItem {
  return {
    id: suggestion.id,
    label: suggestion.label,
    auxiliaryData: { group: GROUP_HEADING[suggestion.kind], suggestion },
  };
}

/** Astryx types a trigger's items as bare `SearchableItem`; every item it hands back is one of ours. */
function suggestionOf(item: SearchableItem): MentionSuggestion {
  return (item as MentionItem).auxiliaryData!.suggestion;
}

/**
 * The chip a pick becomes: its `value` is what the composer serializes, so the sent text holds
 * the server's `insert` exactly; the reader sees the short `chip` form instead.
 */
export function mentionToken(suggestion: MentionSuggestion): ChatComposerToken {
  return { value: suggestion.insert, label: suggestion.chip };
}

/**
 * An async `SearchSource` whose answers are only ever the latest search's.
 *
 * Astryx sets whatever a search resolves to, with no check of its own that the search is still
 * the current one, so the guard lives here: a search superseded by a newer one or ended by
 * `cancel()` aborts its request and its promise never settles. A failed search answers `[]`,
 * which Astryx shows as its empty text. `bootstrap` is not used by the trigger menu.
 */
export function createMentionSource(search: MentionSearch, delayMs: number): SearchSource<MentionItem> {
  type Attempt = { timer: ReturnType<typeof setTimeout> | null; controller: AbortController };
  let current: Attempt | null = null;
  const cancel = () => {
    if (current === null) return;
    if (current.timer !== null) clearTimeout(current.timer);
    current.controller.abort();
    current = null;
  };
  return {
    bootstrap: () => [],
    cancel,
    search(query) {
      cancel();
      const attempt: Attempt = { timer: null, controller: new AbortController() };
      current = attempt;
      return new Promise<MentionItem[]>((resolve) => {
        const answer = (items: MentionItem[]) => {
          if (current === attempt) resolve(items);
        };
        attempt.timer = setTimeout(() => {
          attempt.timer = null;
          search(query, attempt.controller.signal).then(
            (suggestions) => { answer(suggestions.map(itemOf)); },
            () => { answer([]); },
          );
        }, delayMs);
      });
    },
  };
}

function MentionRow({ suggestion }: { suggestion: MentionSuggestion }) {
  return (
    <span className={styles.item} data-nc-mention={suggestion.kind}>
      <span className={styles.label}>{suggestion.label}</span>
      {suggestion.detail !== null && <span className={styles.detail}>{suggestion.detail}</span>}
    </span>
  );
}

export function mentionTrigger(source: SearchSource<MentionItem>): ChatComposerTrigger {
  return {
    character: '@',
    searchSource: source,
    menuLabel: 'Mention',
    emptySearchResultsText: 'Nothing in this area matches',
    renderItem: (item) => <MentionRow suggestion={suggestionOf(item)} />,
    onSelect: (item) => mentionToken(suggestionOf(item)),
  };
}

/**
 * The `@` trigger, or `undefined` where `search` is `null` (a composer no Planner reads).
 *
 * Stable for as long as `search` stays non-null, whatever its identity: `useTriggerMenu`
 * compares the active trigger by identity on every input event, so a new object per render
 * would close the menu mid-query. Each search reads the latest `search`, so a change of
 * area or track applies from the next keystroke.
 */
export function useMentionTrigger(search: MentionSearch | null): ChatComposerTrigger | undefined {
  const searchRef = useRef(search);
  searchRef.current = search;
  const enabled = search !== null;
  return useMemo(() => {
    if (!enabled) return undefined;
    const latest: MentionSearch = (query, signal) => searchRef.current?.(query, signal) ?? Promise.resolve([]);
    return mentionTrigger(createMentionSource(latest, MENTION_SEARCH_DELAY_MS));
  }, [enabled]);
}
