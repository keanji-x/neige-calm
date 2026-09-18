// A run of tool calls as Astryx's `ChatToolCalls`, with what the reader did to
// it held outside the vendor's element.
//
// ── Why the open state is not the vendor's ────────────────────────────────
//
// Astryx 0.1.3 keeps two things in component state and nowhere else: whether
// the group is open (`internalExpanded`, unless `isExpanded` is passed) and,
// per row, whether its failure detail is open (`CallRow`'s `isDetailOpen`,
// which has no prop at all). Both are gone the moment their element unmounts —
// and it does unmount in the ordinary course of a conversation. A refetch of
// the newest 300-row page shifts the window by however many rows arrived, and
// the run the reader had open can be left with one of its calls (a line, not a
// group — `ChatThread`), with none of them, or with all but the one they were
// reading, whose row the vendor drops while the group stays; *Load earlier*
// then brings the rest back. Without this module the reader got the same run
// back closed, or the same row back with the failure they were reading folded
// away. Rendering a lone call through the vendor instead would not have
// helped: with one call it draws the row at a different place in its own
// tree, so the row is rebuilt — and its detail closed — twice over.
//
// So the run's state lives in `ChatThread`, keyed by the run's carried key
// (`keyTranscriptGroups`, which remembers a run by its calls' ids for as long
// as the conversation is open, so a run that left the window whole is the
// same run when its ids come back), and this component is the seam. For the
// group it is an ordinary controlled input: `isExpanded` in, `onExpandedChange`
// out. For the details, which the vendor lets nothing control, it does two
// things:
//
//  - it **watches** the reader toggle a row — the vendor makes exactly the rows
//    that carry a `resultDetail` into `role="button"`s, in `calls` order, and
//    toggles on click and on Enter/Space — and records which detail is open
//    by reading whether that call's detail is in the DOM *before* the toggle
//    lands (`data-nc-detail`, stamped by `ChatThread` on the detail it hands
//    the vendor), so the record is a fact about the vendor's state and not a
//    count of presses;
//  - when the vendor has **rebuilt** a row — it keys rows on the call's own
//    key, so that is when this element mounts and when a call joins the group,
//    including a call a page shift dropped and *Load earlier* restored — it
//    presses, once, on the reader's behalf, the rows whose detail the record
//    says is open and the vendor is not showing.
//
// The press is a DOM `click()` on the vendor's own button, which is the one
// door the vendor leaves open. It is dispatched from a layout effect, so the
// state it flips is applied before the frame paints, and the watcher above is
// told to ignore it. It is the least this module can do and still hand the
// reader back what they had; a vendor with a controllable detail would let the
// whole second half of this file go.
//
// ── Where focus goes when the element under it goes ───────────────────────
//
// The same page shift can take the element the reader's focus is in: the
// row of a call, when the call leaves its run; the run's whole element, when
// the run leaves the window or shrinks to one call (`ChatThread` draws a
// lone call as its own line, not through this component). The engine then
// drops focus on `<body>` — the top of the page, for the next Tab — while
// the conversation is still on screen around where the reader was. So
// `useToolCallFocus`, which `ChatThread` mounts on the transcript's element,
// notes from the transcript's own focus events which run, and which call's
// row or open detail in it, holds focus; and after a commit that no longer
// shows that element it puts focus, without scrolling, on the nearest thing
// that still stands for where the reader was:
//
//  1. the same call, wherever it is drawn now — its row, if its run is open;
//     its run's header, if closed; its line, if it is now a run of one;
//  2. else the same run — its header, or the line of the one call it has left;
//  3. else the nearest surviving run — its header, or its line if it is a
//     run of one — the first run after where the old one stood, or the last
//     before it;
//  4. else the entry that now stands where the run stood — a message, which
//     is no control but is the reader's place in the conversation, and is
//     where the next Tab should go on from.
//
// Where the run stood is read off what survives around it: just before the
// first entry after it that is still shown, else after everything. When
// nothing shown before survives, the transcript cannot say. The one way its
// window is replaced whole is a refetch of the newest page past everything
// shown (*Load earlier* keeps what is shown), and that only moves forward,
// so the run stood before all of it — unless every entry now shown is
// stamped earlier than every entry that was, when the window went back and
// it stood after. `atMs` is read for that contradiction only, as two ranges
// and never as an order: it is not one clock, and not what the transcript
// is sorted by, so ranges that touch or overlap leave the default standing.
//
// The note lives in that hook, on the host that survives, and not in
// `ToolCallGroup`: an effect in the element being unmounted does not run for
// the commit that unmounts it, and the two losses above that take the whole
// element are exactly the ones it could never see. One note, one landing.
//
// A line or a message takes focus only for the landing: it is lent
// `tabindex="-1"` — never in the tab order — and `data-nc-landing`, which
// takes the focus ring off it (a ring promises keyboard input, and these
// take none; `.composer` makes the same argument for its perch), for as
// long as it holds focus. Both are taken back when the reader moves on; at
// the first commit after which it holds focus no longer — a lent line whose
// run came back whole, or that the transcript emptied around, sends no blur
// the note sees; and with the transcript, when that unmounts. Nothing is
// ever landed on a hidden element.
//
// Only then: a reader who had moved on — to the composer, another run, or
// nowhere at all by clicking off — has focus where they put it and it is not
// touched, and neither is focus another effect in the same commit put
// somewhere real. What tells "the element went" from "the reader left" is
// React itself: it dispatches no event for a node it is removing (React DOM
// disables its event system for the mutation phase of every commit), so the
// `blur` the note sees is always the reader's — or the engine's own fixup,
// which comes after the commit and therefore after the landing. Were that
// ever to change, the blur would clear the note and the landing would not
// happen; focus would fall to `<body>` as it did before, and nothing would
// be taken from anyone. The one loss that is not a removal is a run closed
// under the focused row by a press that did not first take focus (assistive
// software, a script): the vendor keeps the rows in the DOM and this
// module's stylesheet hides them, so the landing is the header, and no
// hidden element keeps focus.
//
// And the note itself is kept across a commit only while focus is still in
// the run it names. The element can go in a commit in which another effect
// puts focus somewhere real: nothing is taken from there, but no blur has
// reached the note either, and were it kept it would outlive the focus it
// describes — a reader who then clicks off, onto nothing, blurs the
// composer and not the transcript, and the next commit would find focus on
// `<body>` and land them back on a tool they had left. So after every
// commit, focus in the noted run keeps the note (a later commit that takes
// the element then still has it to land from), and focus anywhere else
// ends it; only a focus event writes another.
//
// The transcript is one instance per conversation (`key={open.id}` at the
// router), so a switch takes the note with the instance: whoever switched
// did so from somewhere, and that somewhere keeps focus.
//
// ── What a failed call says to assistive technology ───────────────────────
//
// The vendor draws a failed call's status as a colored `aria-hidden` icon
// and puts the failure text in that icon's `title`; neither reaches the
// button's accessible name or description, so a reader not looking at the
// color is told "Ran npm test" and no more. So every failed call's row
// carries the word `Failed` for its name, out of sight, in the one slot the
// vendor gives the host inside a row (`stats`); and the header, which has
// no slot, is described by the latest call's while it is closed on a failed
// latest call — an `aria-describedby` this component sets on the vendor's
// element after every commit, as `ui/mobile-list` does, and takes off when
// the header no longer draws that call: open, it names the count, and a
// done or running latest call did not fail. The failure text itself stays
// behind the row, for the reader to open.

import { useId, useLayoutEffect, useRef, type FocusEvent, type KeyboardEvent, type RefObject, type SyntheticEvent } from 'react';
import { ChatToolCalls, type ChatToolCallItem } from '@astryxdesign/core/Chat';

import styles from './activity-groups.module.css';
import type { ConversationActivity } from '../../../../../core/domain/conversation.ts';
import type { KeyedTranscriptGroup } from '../../../../../core/domain/conversation-groups.ts';

/** What the reader did to one run of calls, kept across the vendor's element. */
export type ToolCallGroupUi = Readonly<{
  /** Whether the group is open. */
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

/**
 * Whether the group, as the reader sees it, shows a call still running — the
 * vendor's spinner — so `ChatThread` can keep its own `Working` mark down when
 * it would only repeat that. Closed, the header draws the latest call alone;
 * open, every row draws its own state, so an earlier call still running is
 * visible only then.
 */
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
  /* The calls the vendor had drawn rows for as of the last replay. Rows are
     keyed on the call, so they are rebuilt exactly when this changes; and a
     replay is once per rebuild, not once per run of the effect: StrictMode
     runs the mount effect twice, and the first press has not landed when the
     second run would read the DOM and press again, closing what it opened. */
  const drawn = useRef<string | null>(null);
  const { expanded, openedDetails } = ui;
  /* After every commit rather than on a dependency: `calls` is a fresh array
     each render, and telling a commit that rebuilt rows from one that did not
     is the cheap comparison below, done here rather than by React. */
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

/**
 * The transcript-level half of "where focus goes when the element under it
 * goes" (the note at the top of this file). `entries` is this render's
 * transcript, in order; `callsOf` is how the host turns a run's activities
 * into what it hands the vendor, so the row/call correspondence read here is
 * the same one `ToolCallGroup` reads; `uiOf` is the host's record of whether
 * a run is open, which is what decides whether its rows are shown.
 */
export function useToolCallFocus(
  entries: readonly KeyedTranscriptGroup[],
  callsOf: (activities: readonly ConversationActivity[]) => readonly ChatToolCallItem[],
  uiOf: (run: string) => ToolCallGroupUi,
): ToolCallFocusHost {
  const ref = useRef<HTMLDivElement | null>(null);
  const note = useRef<FocusNote | null>(null);
  /* The last transcript that was committed — where a run that is gone
     *stood*, for landings 3 and 4, and when it was shown, for a window that
     shares nothing with it. */
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
    /* A loan lasts as long as the landing holds focus. The reader moving on
       is a blur `onBlur` answers; the lent element going — its run back
       whole, the transcript emptied around it — is not (React dispatches no
       event for a node it removes), and is answered here, whether focus went
       to `<body>` with it or another effect put it somewhere real. */
    if (lent.current !== null && active !== lent.current) takeBack();
    if (thread === null) { note.current = null; return; }
    if (focused === null) return;

    /* Does the note still describe focus? Held: focus is in the element of
       the run it names — the note stands, unless the run is now closed under
       the focused row and the stylesheet has hidden it, when the header is
       the landing. Dropped: the element went, and the engine put focus on
       `<body>` — the landing. Anything else is where the reader, or another
       effect, put it: nothing is taken from there, and the note is over. */
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
      /* Only a run's elements are noted: a message, or anything else, is not
         this module's to land. */
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

/** The landing, in the order the note at the top of this file gives; `null` only when there is nothing to land on. */
function landingFor(
  thread: HTMLElement,
  entries: readonly KeyedTranscriptGroup[],
  was: readonly KeyedTranscriptGroup[],
  focused: FocusNote,
  callsOf: (activities: readonly ConversationActivity[]) => readonly ChatToolCallItem[],
  uiOf: (run: string) => ToolCallGroupUi,
): HTMLElement | null {
  /* 1 and 2: the same call wherever it is drawn now, else the same run. */
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
  /* Where the run stood: just before the first entry after it that is still
     shown; at the end when only entries before it are; and when nothing is,
     before everything — the window moved past it — unless it went back. */
  const survives = new Set(entries.map((entry) => entry.key));
  const keys = was.map((entry) => entry.key);
  const next = keys.slice(keys.indexOf(focused.run) + 1).find((key) => survives.has(key));
  const stood = next !== undefined ? entries.findIndex((entry) => entry.key === next)
    : keys.some((key) => survives.has(key)) || windowWentBack(was, entries) ? entries.length
    : 0;
  /* 3: the nearest run, after first — its header, or its line if it is a
     run of one. 4: whatever stands there now. */
  const near = entries.slice(stood).find(isRun) ?? entries.slice(0, stood).findLast(isRun);
  const stand = near ?? entries[Math.min(stood, entries.length - 1)];
  if (stand === undefined) return null;
  const root = entryRoot(thread, stand.key);
  return root === null || near === undefined || !isGroup(near) ? root : headerOf(root);
}

/**
 * For a transcript that shares no entry with the one before it: whether it
 * lies wholly before it in time — every entry now shown stamped earlier than
 * every entry that was. Two ranges, not an order (the note at the top of
 * this file): ranges that touch or overlap are not a contradiction.
 */
function windowWentBack(was: readonly KeyedTranscriptGroup[], entries: readonly KeyedTranscriptGroup[]): boolean {
  const before = timesOf(was);
  const now = timesOf(entries);
  return now.length > 0 && before.length > 0 && Math.max(...now) < Math.min(...before);
}

/** Every time stamped on the entries: a message's own, each call of a run's. */
function timesOf(entries: readonly KeyedTranscriptGroup[]): readonly number[] {
  return entries.flatMap((entry) => entry.activities?.map((activity) => activity.atMs) ?? [entry.entry.atMs]);
}
