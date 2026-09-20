// A run of tool calls as Astryx's `ChatToolCalls`, with what the reader did to it
// (open state, opened failure details, focus) held outside the vendor's element.

import { useId, useLayoutEffect, useRef, type FocusEvent, type KeyboardEvent, type RefObject, type SyntheticEvent } from 'react';
import { ChatToolCalls, type ChatToolCallItem } from '@astryxdesign/core/Chat';

import styles from './activity-groups.module.css';
import type { ConversationActivity } from '../../../../../core/domain/conversation.ts';
import type { KeyedTranscriptGroup } from '../../../../../core/domain/conversation-groups.ts';

/** What the reader did to one run of calls, kept across the vendor's element. */
export type ToolCallGroupUi = Readonly<{
  expanded: boolean;
  /** The calls, by their `key`, whose failure detail the reader has open. */
  openedDetails: ReadonlySet<string>;
}>;

/** A run the reader has not touched: closed, every detail folded. */
export function untouchedToolCallGroup(): ToolCallGroupUi {
  return { expanded: false, openedDetails: new Set() };
}

/** `openedDetails` with `key` in or out of it, as `open` says. */
export function withDetailOpen(ui: ToolCallGroupUi, key: string, open: boolean): ToolCallGroupUi {
  if (ui.openedDetails.has(key) === open) return ui;
  const openedDetails = new Set(ui.openedDetails);
  if (open) openedDetails.add(key); else openedDetails.delete(key);
  return { ...ui, openedDetails };
}

/** Whether the group, as the reader sees it, shows a call still running (the vendor's spinner), so `ChatThread` can keep its own `Working` mark down. */
export function toolCallGroupShowsRunning(
  calls: readonly ChatToolCallItem[], expanded: boolean,
): boolean {
  const running = (call: ChatToolCallItem | undefined) => call?.status === 'running';
  return expanded ? calls.some(running) : running(calls[calls.length - 1]);
}

export type ToolCallGroupProps = Readonly<{
  /** Two or more; a lone call is `ChatThread`'s own line. Each carries its activity id as `key`. */
  calls: readonly ChatToolCallItem[];
  /** The run's carried key — what `ChatThread` keys this element on — stamped as `data-nc-entry` for `useToolCallFocus`. */
  entry: string;
  ui: ToolCallGroupUi;
  onExpandedChange: (expanded: boolean) => void;
  onDetailOpenChange: (key: string, open: boolean) => void;
  /** The group at the tail of a live transcript — the only one allowed to spin. */
  live: boolean;
}>;

export function ToolCallGroup({ calls, entry, ui, onExpandedChange, onDetailOpenChange, live }: ToolCallGroupProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const id = useId();
  /** The element that says `Failed` for the call at `index`: in its row, and by reference for the header. */
  const failedId = (index: number) => `${id}failed-${index}`;
  /* True while this component is pressing rows itself, so the watcher below
     does not mistake its own presses for the reader's. */
  const replaying = useRef(false);
  /* The calls the vendor had drawn rows for as of the last replay: a replay is once per rebuild, not once per effect run — StrictMode runs the mount effect twice before the first press has landed. */
  const drawn = useRef<string | null>(null);
  const { expanded, openedDetails } = ui;
  /* After every commit rather than on a dependency: `calls` is a fresh array each render. */
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (root === null) return;
    const drawing = calls.map((call) => call.key).join('\u001F');
    if (drawn.current === drawing) return;
    drawn.current = drawing;
    const rows = detailRows(root);
    replaying.current = true;
    try {
      detailCalls(calls).forEach((call, index) => {
        if (call.key !== undefined && openedDetails.has(call.key) && !detailIsOpen(root, call.key)) rows[index]?.click();
      });
    } finally {
      replaying.current = false;
    }
  });

  /* After every commit: closed, the header draws the latest call, and which
     call that is and whether it failed both change under the same element. */
  useLayoutEffect(() => {
    const header = rootRef.current === null ? null : headerOf(rootRef.current);
    if (header === null) return;
    const latest = calls.length - 1;
    if (!expanded && calls[latest]?.status === 'error') header.setAttribute('aria-describedby', failedId(latest));
    else header.removeAttribute('aria-describedby');
  });

  const noteToggle = (event: SyntheticEvent<HTMLElement>) => {
    const root = rootRef.current;
    if (replaying.current || root === null || !(event.target instanceof Element)) return;
    const row = event.target.closest<HTMLElement>('[role="button"]');
    /* The header is the vendor's other button, and it has `aria-expanded`;
       it reports through `onExpandedChange`. */
    if (row === null || row.hasAttribute('aria-expanded') || !root.contains(row)) return;
    const call = detailCalls(calls)[detailRows(root).indexOf(row)];
    if (call?.key === undefined) return;
    onDetailOpenChange(call.key, !detailIsOpen(root, call.key));
  };

  return (
    <ChatToolCalls
      ref={rootRef}
      className={styles.group}
      role="group"
      aria-label={`${calls.length} tool calls`}
      isExpanded={expanded}
      onExpandedChange={onExpandedChange}
      onClick={noteToggle}
      onKeyDown={(event: KeyboardEvent<HTMLDivElement>) => {
        if (event.key === 'Enter' || event.key === ' ') noteToggle(event);
      }}
      data-nc-entry={entry}
      {...(live ? { 'data-nc-live': '' } : {})}
      calls={calls.map((call, index) => (call.status !== 'error' ? call
        : { ...call, stats: <span className={styles.srOnly} id={failedId(index)}>Failed</span> }))}
    />
  );
}

/** The calls the vendor makes rows-with-a-button of, in the order it draws them. */
function detailCalls(calls: readonly ChatToolCallItem[]): readonly ChatToolCallItem[] {
  return calls.filter((call) => call.resultDetail != null);
}

/** Those rows, in DOM order — the same order — excluding anything a detail itself contains. */
function detailRows(root: HTMLElement): readonly HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('[role="button"]:not([aria-expanded])')]
    .filter((row) => row.closest('[data-nc-detail]') === null);
}

/** Whether the vendor is showing `key`'s detail: it mounts the detail only while open. */
function detailIsOpen(root: HTMLElement, key: string): boolean {
  return [...root.querySelectorAll('[data-nc-detail]')].some((detail) => detail.getAttribute('data-nc-detail') === key);
}

/** The key of the call whose row, or whose open detail, `element` is in; `null` for the header or anything else. */
function callAround(root: HTMLElement, calls: readonly ChatToolCallItem[], element: Element): string | null {
  const detail = element.closest('[data-nc-detail]');
  if (detail !== null && root.contains(detail)) return detail.getAttribute('data-nc-detail');
  const row = element.closest<HTMLElement>('[role="button"]:not([aria-expanded])');
  if (row === null || !root.contains(row)) return null;
  return detailCalls(calls)[detailRows(root).indexOf(row)]?.key ?? null;
}

/** The row-with-a-button the vendor draws for call `key`, if it draws one. */
function rowOf(root: HTMLElement, calls: readonly ChatToolCallItem[], key: string): HTMLElement | undefined {
  const index = detailCalls(calls).findIndex((call) => call.key === key);
  return index === -1 ? undefined : detailRows(root)[index];
}

/** The group's disclosure header — the vendor's `role="button"` that carries `aria-expanded`. */
function headerOf(root: HTMLElement): HTMLElement | null {
  return root.querySelector<HTMLElement>('[aria-expanded]');
}

/** The transcript's element for the entry keyed `key`: every child `ChatThread` draws for an entry carries `data-nc-entry`. */
function entryRoot(thread: HTMLElement, key: string): HTMLElement | null {
  for (const child of thread.children) {
    if (child instanceof HTMLElement && child.getAttribute('data-nc-entry') === key) return child;
  }
  return null;
}

/** A run of calls, of any length: a group, or `ChatThread`'s line for a run of one. */
function isRun(entry: KeyedTranscriptGroup): boolean {
  return entry.activities !== null;
}

/** A run of two or more calls — what `ToolCallGroup` draws; one call is `ChatThread`'s line. */
function isGroup(entry: KeyedTranscriptGroup): boolean {
  return entry.activities !== null && entry.activities.length > 1;
}

/** Where the reader's focus is, as the transcript's last focus event said: in run `run`, on call `call`'s row or open detail — or on the run's header, `null`. */
type FocusNote = Readonly<{ run: string; call: string | null }>;

export type ToolCallFocusHost = Readonly<{
  /** For the transcript's element — the parent of every entry's element. */
  ref: RefObject<HTMLDivElement | null>;
  onFocus: (event: FocusEvent<HTMLDivElement>) => void;
  onBlur: (event: FocusEvent<HTMLDivElement>) => void;
}>;

/** The transcript-level half of where focus goes when the element under it goes. */
export function useToolCallFocus(
  entries: readonly KeyedTranscriptGroup[],
  callsOf: (activities: readonly ConversationActivity[]) => readonly ChatToolCallItem[],
  uiOf: (run: string) => ToolCallGroupUi,
): ToolCallFocusHost {
  const ref = useRef<HTMLDivElement | null>(null);
  const note = useRef<FocusNote | null>(null);
  /* The last committed transcript — where a run that is gone stood. */
  const shown = useRef<readonly KeyedTranscriptGroup[]>([]);
  /* The element lent `tabindex` and `data-nc-landing` for a landing, for as
     long as it holds focus. */
  const lent = useRef<HTMLElement | null>(null);
  const takeBack = () => {
    const element = lent.current;
    if (element === null) return;
    lent.current = null;
    element.removeAttribute('tabindex');
    element.removeAttribute('data-nc-landing');
  };

  /* After every commit, and after every child's own layout effect — a
     `ToolCallGroup`'s replay presses rows but moves no focus. */
  useLayoutEffect(() => {
    const thread = ref.current;
    const was = shown.current;
    shown.current = entries;
    const focused = note.current;
    const active = document.activeElement;
    /* The lent element going (its run back whole, the transcript emptied around it) sends no blur — React dispatches no event for a node it removes — so it is answered here. */
    if (lent.current !== null && active !== lent.current) takeBack();
    if (thread === null) { note.current = null; return; }
    if (focused === null) return;

    /* Dropped: the element went and the engine put focus on `<body>`. Held: focus is still in the noted run. Anything else is where the reader or another effect put it, and the note is over. */
    const dropped = active === null || active === document.body;
    const sameRun = entries.find((entry) => entry.key === focused.run);
    const sameRunRoot = sameRun === undefined ? null : entryRoot(thread, sameRun.key);
    const held = sameRunRoot !== null && sameRunRoot.contains(active);
    const closedUnder = held && sameRun !== undefined && focused.call !== null && isGroup(sameRun)
      && !uiOf(sameRun.key).expanded;
    if (held && !closedUnder) return;
    note.current = null;
    if (!dropped && !closedUnder) return;

    const target = landingFor(thread, entries, was, focused, callsOf, uiOf);
    if (target === null) return;
    if (!target.hasAttribute('tabindex')) {
      target.tabIndex = -1;
      target.setAttribute('data-nc-landing', '');
      lent.current = target;
    }
    target.focus({ preventScroll: true });
  });
  /* With the transcript goes its last landing: the lent element is being
     removed, and focus with it, blur or no blur. */
  useLayoutEffect(() => takeBack, []);

  return {
    ref,
    onFocus: (event) => {
      const thread = ref.current;
      const child = event.target.closest<HTMLElement>('[data-nc-entry]');
      const entry = thread === null || child === null || child.parentElement !== thread ? undefined
        : entries.find((candidate) => candidate.key === child.getAttribute('data-nc-entry'));
      if (child === null || entry?.activities == null) { note.current = null; return; }
      note.current = {
        run: entry.key,
        call: isGroup(entry) ? callAround(child, callsOf(entry.activities), event.target) : entry.activities[0].id,
      };
    },
    onBlur: (event) => {
      note.current = null;
      if (event.target === lent.current) takeBack();
    },
  };
}

/** The landing; `null` only when there is nothing to land on. */
function landingFor(
  thread: HTMLElement,
  entries: readonly KeyedTranscriptGroup[],
  was: readonly KeyedTranscriptGroup[],
  focused: FocusNote,
  callsOf: (activities: readonly ConversationActivity[]) => readonly ChatToolCallItem[],
  uiOf: (run: string) => ToolCallGroupUi,
): HTMLElement | null {
  /* The same call wherever it is drawn now, else the same run. */
  const byCall = focused.call === null ? undefined
    : entries.find((entry) => entry.activities?.some((activity) => activity.id === focused.call) ?? false);
  const same = byCall ?? entries.find((entry) => entry.key === focused.run);
  if (same?.activities != null) {
    const root = entryRoot(thread, same.key);
    if (root === null) return null;
    if (!isGroup(same)) return root;
    const row = focused.call !== null && uiOf(same.key).expanded ? rowOf(root, callsOf(same.activities), focused.call) : undefined;
    return row ?? headerOf(root);
  }
  /* Where the run stood: before the first still-shown entry after it; at the end when only earlier entries survive; before everything when none do — unless the window went back. */
  const survives = new Set(entries.map((entry) => entry.key));
  const keys = was.map((entry) => entry.key);
  const next = keys.slice(keys.indexOf(focused.run) + 1).find((key) => survives.has(key));
  const stood = next !== undefined ? entries.findIndex((entry) => entry.key === next)
    : keys.some((key) => survives.has(key)) || windowWentBack(was, entries) ? entries.length
    : 0;
  /* Else the nearest run, after first — its header, or its line if a run of one; else whatever stands there now. */
  const near = entries.slice(stood).find(isRun) ?? entries.slice(0, stood).findLast(isRun);
  const stand = near ?? entries[Math.min(stood, entries.length - 1)];
  if (stand === undefined) return null;
  const root = entryRoot(thread, stand.key);
  return root === null || near === undefined || !isGroup(near) ? root : headerOf(root);
}

/** Whether a transcript sharing no entry with the one before lies wholly before it in time. Two ranges, not an order: ranges that touch or overlap are not a contradiction. */
function windowWentBack(was: readonly KeyedTranscriptGroup[], entries: readonly KeyedTranscriptGroup[]): boolean {
  const before = timesOf(was);
  const now = timesOf(entries);
  return now.length > 0 && before.length > 0 && Math.max(...now) < Math.min(...before);
}

/** Every time stamped on the entries: a message's own, each call of a run's. */
function timesOf(entries: readonly KeyedTranscriptGroup[]): readonly number[] {
  return entries.flatMap((entry) => entry.activities?.map((activity) => activity.atMs) ?? [entry.entry.atMs]);
}
