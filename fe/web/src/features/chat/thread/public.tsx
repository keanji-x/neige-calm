// The conversation itself: a transcript and the box you write into. The unit is the
// exchange — one thing you said and everything that came back.

import { useEffect, useLayoutEffect, useMemo, useRef, type ReactNode, type Dispatch, type SetStateAction } from 'react';
import { createPortal } from 'react-dom';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
  ChatSystemMessage,
  type ChatComposerTrigger,
  type ChatToolCallItem,
} from '@astryxdesign/core/Chat';
import { Code } from '@astryxdesign/core/Code';
import { Markdown } from '@astryxdesign/core/Markdown';
import { createStaticSource } from '@astryxdesign/core/Typeahead';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { EdgeNavigator } from '../../../ui/edge-navigation/public.tsx';
import { observeResize } from '../../../ui/edge-navigation/resize.ts';
import { drawerSeamAround } from '../../../ui/drawer/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';

import { activityLabelOf, cardActivityOf, type CardActivity } from '../../../../../core/domain/activity.ts';
import { foldQuietSyncs } from '../../../../../core/domain/conversation-quiet-sync.ts';
import {
  isQueuedConversationTurn, opensAfterGap, opensExchange,
  type Conversation, type ConversationActivity, type SendOutcome, type TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import { QuietSyncFold } from './quiet-sync.tsx';
import styles from './thread.module.css';
import {
  ToolCallGroup, toolCallGroupShowsRunning, untouchedToolCallGroup, useToolCallFocus, withDetailOpen,
  type ToolCallGroupUi,
} from './activity-groups.tsx';
import {
  groupTranscriptActivities, keyTranscriptGroups, noTranscriptGroupKeys, type TranscriptGroupKeys,
} from '../../../../../core/domain/conversation-groups.ts';

export type ChatThreadProps = Readonly<{
  conversation: Conversation;
  /** Messages and the actions between them, in the order they happened. */
  turns: readonly TranscriptEntry[];
  /** The sender's own turn in flight, shown before the kernel's next tick can say so. */
  pending?: boolean;
  /** The kernel's per-card verdicts for this conversation's track; a caller with no overlay passes `{}`. */
  cards: Readonly<Record<string, CardActivity>>;
  /** The drawer's local wedge fact. A wedged planner's kernel verdict is still `working`, so a thread that did not know it was stuck would spin beside "This conversation is stuck". */
  stalled: boolean;
}>;

export function ChatThread({ conversation, turns, pending = false, cards, stalled }: ChatThreadProps) {
  /* The live mark is the sender's pending send or the kernel's verdict — never `conversation.state`, which sits at `turn_pending`/`running` long after a turn ended. The local wedge outranks both. */
  const live = !stalled && (pending || cardActivityOf({ cards }, conversation.id) === 'working');
  const lastTurn = turns[turns.length - 1];
  const endRef = useRef<HTMLDivElement | null>(null);
  /** The box every marker lookup starts from. Not `.thread` itself: the stylesheet's `> * + *` rules space that element's children. */
  const frameRef = useRef<HTMLDivElement | null>(null);
  const exchanges = useMemo(() => exchangesOf(turns), [turns]);
  /* Drawn by block, not by entry: a quiet-sync fold is one line. The index map keeps `opensExchange`, `opensAfterGap` and the live-mark rule reading positions in `turns`. */
  const blocks = useMemo(() => foldQuietSyncs(turns), [turns]);
  const indexOf = useMemo(
    () => new Map(turns.map((entry, index) => [entry, index] as const)), [turns],
  );
  // Quiet syncs remain a single top-level boundary; their existing disclosure
  // owns its children. Ordinary tools on either side must never join through it.
  const quietBlocks = useMemo(() => new Map(blocks
    .filter((block) => block.kind === 'quiet-sync').map((block) => [block.id, block] as const)), [blocks]);
  const visibleTurns = useMemo(() => blocks.map((block) => block.kind === 'entry'
    ? block.entry : block.entries[0]), [blocks]);
  /* Run keys are read from the memory of the last *committed* transcript, advanced only in a layout effect: React renders transcripts it then throws away, and a memory advanced during render would hold keys for calls nobody saw. */
  const committedGroupKeys = useRef<TranscriptGroupKeys>(noTranscriptGroupKeys());
  const keyedGroups = useMemo(
    () => keyTranscriptGroups(groupTranscriptActivities(visibleTurns), committedGroupKeys.current),
    [visibleTurns],
  );
  const transcriptGroups = keyedGroups.groups;
  useLayoutEffect(() => {
    committedGroupKeys.current = keyedGroups.memory;
  }, [keyedGroups]);
  /* What the reader did to each run, by the run's carried key — held here because the vendor's element does not survive a page shift. A key is issued once and never reissued. */
  const [groupUi, setGroupUi] = useState<ReadonlyMap<string, ToolCallGroupUi>>(() => new Map());
  const updateGroupUi = (key: string, update: (previous: ToolCallGroupUi) => ToolCallGroupUi) => {
    setGroupUi((previous) => new Map(previous).set(key, update(previous.get(key) ?? untouchedToolCallGroup())));
  };
  /* Where focus goes when the element under it goes; held on the transcript's element, which outlives every run's. */
  const focus = useToolCallFocus(
    transcriptGroups.filter(({ entry }) => entry.author !== 'turn' || entry.status !== 'completed'),
    (activities) => activities.map(toolCallOf),
    (key) => groupUi.get(key) ?? untouchedToolCallGroup(),
  );
  /* Whether the end of the transcript already says "working", so the placeholder mark stays down: a run's *visible* rows count — the latest call when closed, every call when open. */
  const tail = transcriptGroups[transcriptGroups.length - 1];
  const tailQuiet = tail === undefined ? undefined : quietBlocks.get(tail.entry.id);
  const tailCarriesLiveMark = tail === undefined ? false
    : tailQuiet !== undefined ? tailQuiet.entries.some((entry) => entry === lastTurn)
    : tail.activities === null ? tail.entry.author === 'agent'
    : tail.activities.length === 1 ? tail.activities[0].state === 'running'
    : toolCallGroupShowsRunning(tail.activities.map(toolCallOf), groupUi.get(tail.key)?.expanded ?? false);
  /* The rail is portalled into the drawer's seam (`.drawer` is `overflow: hidden`, so a descendant cannot reach it). Held in state because a portal needs the node at render time. */
  const [railSeam, setRailSeam] = useState<HTMLElement | null>(null);
  /* The frame is not rendered on an empty transcript, so the first turn arriving is the one edge that creates it under a live component. */
  const hasTranscript = turns.length > 0;
  useLayoutEffect(() => {
    setRailSeam(drawerSeamAround(frameRef.current));
  }, [hasTranscript]);
  const railShown = exchanges.length > 0 && railSeam !== null;
  const [active, setActive] = useState<string | null>(null);
  /** Re-derive the lit dot from the painted boxes; a no-op before the rail effect has installed it. */
  const readActive = useRef<() => void>(() => {});
  /** Whether the reader is parked at the end of the transcript — the only state in which a newly appended turn may move the pane. */
  const followsNewest = useRef(true);
  /** The turn at the end of the transcript as of this render — what the follow
   *  effect below both depends on and decides by. */
  const newestId = lastTurn?.id;
  /** The newest turn as of the last run of the follow effect, so *Load earlier* is not mistaken for an arrival. Updated on every run, including runs that decline to scroll. */
  const followedTo = useRef<string | undefined>(undefined);

  /* Follow the newest turn only for a reader already at the bottom, and only when the last turn's id changed — not the count: *Load earlier* grows the count, and a collapsed `Thought` changes the id without it. A pane resize moves the reader without a `scroll`, so the same measurement runs from a `ResizeObserver`. Write the pane's own `scrollTop`: `scrollIntoView` pans every ancestor scrollport. */
  useEffect(() => {
    const end = endRef.current;
    if (end == null) return;
    const scroller = end.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller == null) return;
    const arrived = newestId !== followedTo.current;
    followedTo.current = newestId;
    if (arrived && followsNewest.current) scroller.scrollTop = scroller.scrollHeight;
    const measure = () => {
      followsNewest.current = scroller.scrollHeight - scroller.scrollTop
        - scroller.clientHeight <= FOLLOW_BOTTOM_SLACK_PX;
    };
    scroller.addEventListener('scroll', measure, { passive: true });
    const unobserve = observeResize(scroller, measure);
    return () => {
      scroller.removeEventListener('scroll', measure);
      unobserve();
    };
  }, [turns.length, newestId]);

  /* The lit dot is the last exchange whose opening marker sits at or above an edge: the pane's top while a pane-height of scroll remains, sliding to the bottom as it runs out (a hard switch jumped the mark by a pane's worth). Evaluated on every scroll rather than by an observer. `read()` stops at a zero-height pane, and that guard lives only there so a pane mounted at zero height still gets its listeners. */
  const exchangeKey = JSON.stringify(exchanges.map((exchange) => exchange.id));
  useEffect(() => {
    const frame = frameRef.current;
    if (!railShown || frame === null) return;
    const scroller = frame.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller === null) return;
    const markers = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')];
    if (markers.length === 0) return;

    const read = () => {
      if (scroller.clientHeight === 0) return;
      const pane = scroller.getBoundingClientRect();
      /* How far the pane can still travel, and so how far the edge has slid — capped by how far the pane has actually scrolled. */
      const remaining = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      const slid = Math.min(scroller.scrollTop, Math.max(0, pane.height - remaining));
      const edge = Math.min(pane.bottom, pane.top + ACTIVE_MARKER_SLACK_PX + slid);
      let current = markers[0];
      for (const marker of markers) {
        if (marker.getBoundingClientRect().top > edge) break;
        current = marker;
      }
      const id = current?.dataset.ncExchange;
      if (id !== undefined) setActive(id);
    };
    readActive.current = read;

    let queued: number | null = null;
    const onScroll = () => {
      if (queued !== null) return;
      queued = requestAnimationFrame(() => { queued = null; read(); });
    };
    /* A resize moves every marker relative to the pane's edges without emitting a `scroll`. */
    const unobserve = observeResize(scroller, read);
    scroller.addEventListener('scroll', onScroll, { passive: true });
    read();
    return () => {
      readActive.current = () => {};
      unobserve();
      scroller.removeEventListener('scroll', onScroll);
      if (queued !== null) cancelAnimationFrame(queued);
    };
  }, [railShown, exchangeKey]);

  /* One entry, in the position `turns` gives it; `index` is what the exchange, gap and live-mark rules are stated over. */
  const renderEntry = (turn: TranscriptEntry, key = turn.id, showLive = live): ReactNode => {
    const index = indexOf.get(turn) ?? -1;
    const last = index === turns.length - 1;
    if (turn.author === 'activity') {
      return <ActivityLine key={turn.id} entry={key} activity={turn} live={showLive && last} />;
    }
    if (turn.author === 'system') {
      return (
        <div key={turn.id} data-nc-entry={key}>
          {opensAfterGap(turns, index) && index > 0 && (
            <p className={styles.gap}>{clockTime(turn.atMs)}</p>
          )}
          <details
            className={styles.system}
            data-nc-turn="system"
          >
            <summary className={styles.systemSummary} title={turn.text}>
              <span className={styles.systemDisclosure} aria-hidden="true">›</span>
              <span className={styles.systemLabel} data-nc-system-label="">
                · {turn.label} ·
              </span>
            </summary>
            <p className={styles.systemDetail}>{turn.text}</p>
          </details>
        </div>
      );
    }
    if (turn.author === 'turn') {
      /* Only `interrupted` and `failed` paint; `completed` is an anchor. `ChatSystemMessage` forwards no `data-*` and its content span is `nowrap`, so the state hooks and the message sit on this wrapper. */
      if (turn.status === 'completed') return null;
      return (
        <div
          key={turn.id}
          className={styles.outcome}
          data-nc-entry={key}
          data-nc-turn="outcome"
          data-nc-turn-outcome={turn.status}
        >
          <ChatSystemMessage>{turn.status === 'interrupted' ? 'Stopped' : 'Failed'}</ChatSystemMessage>
          {turn.status === 'failed' && turn.message !== undefined && turn.message !== '' && (
            <p className={styles.outcomeDetail} data-nc-turn-outcome-message="">{turn.message}</p>
          )}
          {turn.status === 'failed' && (
            <OutcomeHint code={turn.code} rawStatus={turn.rawStatus} />
          )}
        </div>
      );
    }
    const opens = opensExchange(turns, index);
    return (
      <div
        key={turn.id}
        className={opens ? styles.exchange : undefined}
        data-nc-entry={key}
        {...(opens ? { 'data-nc-exchange': turn.id } : {})}
      >
        {/* A time only where the conversation restarted. */}
        {opensAfterGap(turns, index) && index > 0 && (
          <p className={styles.gap}>{clockTime(turn.atMs)}</p>
        )}
        {turn.author === 'you' ? (
          <>
            {/* The caption is outside the `<p>`: the paragraph is the message verbatim, to a screen reader and to every `getByText`. */}
            <p
              className={styles.said}
              data-nc-turn="you"
              {...(isQueuedConversationTurn(turn) ? { 'data-nc-queued': '' } : {})}
            >{turn.text}</p>
            {/* `alt=""` and `aria-hidden`: the transcript has no description of the image to offer, and the count is said once in text above. */}
            {(turn.attachments ?? []).length > 0 && (
              <ul className={styles.attachments} data-nc-turn-attachments="">
                {(turn.attachments ?? []).map((attachment) => (
                  <li key={attachment.id} className={styles.attachment}>
                    <img src={attachment.url} alt="" />
                  </li>
                ))}
              </ul>
            )}
            {isQueuedConversationTurn(turn) && (
              /* `role="status"`: it appears in response to the reader's own press, answering "did that go anywhere?". */
              <p className={styles.queuedNote} data-nc-queued-note="" role="status">
                Queued · sends when this turn ends
              </p>
            )}
          </>
        ) : (
          <div className={styles.reply} data-nc-turn="agent">
            <Reply text={turn.text} />
            {showLive && last && <ActivityIndicator state="working" />}
          </div>
        )}
      </div>
    );
  };

  if (turns.length === 0) {
    return (
      <div className={styles.empty} data-nc-thread-empty="">
        <p className={styles.emptyLead}>{live ? 'The agent is working.' : 'Nothing said yet.'}</p>
        <p className={styles.emptyHint}>{live ? 'Messages will appear here.' : 'Write below and it starts here.'}</p>
        {live && <ActivityIndicator state="working" />}
      </div>
    );
  }

  return (
    <div className={styles.threadFrame} ref={frameRef}>
      {railShown && railSeam !== null && createPortal(
        <EdgeNavigator
          className={styles.edgeNavigation}
          label="Jump to an exchange"
          items={exchanges.map((item, index) => ({ ...item,
            label: `Jump to exchange ${index + 1}${railLabel(item.text) === '' ? '' : `: ${railLabel(item.text)}`}`,
          }))}
          activeId={active}
          onSelect={(id) => {
            /* The lookup comes first: a marker that is not there leaves the mark untouched. */
            if (!jumpToExchange(frameRef.current, id)) return;
            /* The mark moves on the press, then the rule re-reads the boxes: a write the engine clamps to the current offset fires no `scroll`, so nothing else would correct the press. */
            setActive(id);
            readActive.current();
          }}
        />,
        railSeam,
      )}
      <div className={styles.thread} data-nc-thread="" ref={focus.ref} onFocus={focus.onFocus} onBlur={focus.onBlur}>
        {transcriptGroups.map(({ entry: turn, activities, key }) => {
          const block = quietBlocks.get(turn.id);
          if (block !== undefined) {
            return (
              <div key={block.id} data-nc-entry={key}>
                <QuietSyncFold group={block} time={clockTime(block.atMs)}
                  live={live && block.entries.some((entry) => entry === lastTurn)}>
                  {block.entries.map((entry) => renderEntry(entry, entry.id, false))}
                </QuietSyncFold>
              </div>
            );
          }
          const last = (activities?.at(-1) ?? turn) === lastTurn;
          if (turn.author === 'activity') {
            if (activities !== null && activities.length > 1) {
              return (
                <ToolCallGroup
                  /* Carried by membership, not read off any one call, so the group keeps its element as calls are appended, prepended or dropped. */
                  key={key}
                  entry={key}
                  calls={activities.map(toolCallOf)}
                  ui={groupUi.get(key) ?? untouchedToolCallGroup()}
                  onExpandedChange={(expanded) => updateGroupUi(key, (ui) => ({ ...ui, expanded }))}
                  onDetailOpenChange={(callKey, open) => updateGroupUi(key, (ui) => withDetailOpen(ui, callKey, open))}
                  /* The same `live && last` a lone line answers to: a group the conversation has moved past must not look like it resumed. */
                  live={live && last}
                />
              );
            }
            return <ActivityLine key={turn.id} entry={key} activity={turn} live={live && last} />;
          }
          return renderEntry(turn, key);
        })}
        {/* A reply that has not arrived yet still gets a place to arrive in; the placeholder keeps the one mark visible unless the tail carries it. */}
        {live && !tailCarriesLiveMark && (
          <p className={styles.reply}><ActivityIndicator state="working" /></p>
        )}
        {/* The drawer's one accessible "in motion" fact: every indicator is decorative. It sits after the placeholder because the stylesheet spaces `.thread`'s children by adjacency (`.exchange + *`). */}
        {live && <VisuallyHidden>{activityLabelOf('working')}</VisuallyHidden>}
        <div ref={endRef} aria-hidden="true" />
      </div>
    </div>
  );

}

/** One thing you said and everything that came back — as far as the rail needs
 *  it: something to point at, and the words to call it by. */
type Exchange = Readonly<{ id: string; text: string }>;

/** How far below the pane's top a marker may still count as "scrolled past": absorbs the subpixel gap between the scroll asked for and the one the engine performs, at both ends. */
const ACTIVE_MARKER_SLACK_PX = 4;

/** How far from the bottom still counts as reading the newest turn: 2.6 lines of the reply's 24.75px line box. */
const FOLLOW_BOTTOM_SLACK_PX = 64;

/** One plain sentence for the `codexErrorInfo` values a reader can act on; every other code is shown as the token codex sent. */
const FAILURE_HINTS: Readonly<Record<string, string>> = Object.freeze({
  contextWindowExceeded: 'The conversation no longer fits in the model’s context window.',
  usageLimitExceeded: 'The usage limit for this account has been reached.',
  rateLimitExceeded: 'Requests are being rate-limited; try again in a moment.',
  serverOverloaded: 'The model provider is overloaded; try again in a moment.',
});

function OutcomeHint({ code, rawStatus }: { code: string | undefined; rawStatus: string | undefined }) {
  const hint = code === undefined ? null : (FAILURE_HINTS[code] ?? code);
  if (hint === null && rawStatus === undefined) return null;
  return (
    <p className={styles.outcomeDetail} data-nc-turn-outcome-hint="">
      {hint ?? `Ended with status “${rawStatus}”`}
    </p>
  );
}

function exchangesOf(turns: readonly TranscriptEntry[]): readonly Exchange[] {
  const found: Exchange[] = [];
  turns.forEach((turn, index) => {
    /* `opensExchange` already implies `author === 'you'`; the narrowing below is
       for the type checker, which cannot read that from the domain function. */
    if (!opensExchange(turns, index) || turn.author !== 'you') return;
    found.push({ id: turn.id, text: turn.text });
  });
  return found;
}

/** How much of a prompt a name may carry: enough to tell two questions apart, short enough for a screen reader. */
const RAIL_LABEL_MAX = 60;

/** The prompt as a button may carry it, or `''` where there is nothing to
 *  carry — the ordinal that names it either way is the caller's. */
function railLabel(text: string): string {
  /* Line breaks are the author's and the transcript keeps them; a button's name
     is a single line, so they collapse here and only here. */
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_LABEL_MAX ? line : `${line.slice(0, RAIL_LABEL_MAX - 1)}…`;
}

/** Put the exchange at the top of the drawer's pane by writing that pane's `scrollTop`, never `scrollIntoView` (which pans every ancestor scrollport). Returns whether there was somewhere to go, not whether the pane moved: the engine clamps the write. */
function jumpToExchange(frame: HTMLElement | null, id: string): boolean {
  if (frame === null) return false;
  const marker = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')]
    .find((candidate) => candidate.dataset.ncExchange === id);
  if (marker === undefined) return false;
  const scroller = marker.closest<HTMLElement>('[data-nc-drawer-scroll]');
  if (scroller === null) return false;
  scroller.scrollTop += marker.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
  return true;
}

/** The reply is markdown via Astryx's `Markdown`. `isStreaming` is deliberately not passed: it is a typewriter that withholds text and splits it into spans. `headingLevelStart={3}` because the page owns `<h1>` and its sections `<h2>`. */
function Reply({ text }: { text: string }) {
  return <Markdown density="compact" headingLevelStart={3}>{text}</Markdown>;
}

/** A duration is printed only when the reader felt it; most `item/completed` are a 12ms read. */
const ACTIVITY_DURATION_FLOOR_MS = 1_000;

/** `4.3s` under a minute, `3m 12s` over. The branch is decided on the rounded tenths, or `[59_950, 60_000)` would print `60.0s`. */
function formatActivityDuration(durationMs: number): string {
  const tenths = Math.round(durationMs / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const seconds = Math.round(durationMs / 1_000);
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`;
}

/** Two or more adjacent activities render as the vendor's grouped tool calls; a lone one keeps `ActivityLine`. `key` is the activity id (stable from started to completed); `resultDetail` is wrapped in `data-nc-detail`, which the vendor mounts only while open — that is how `ToolCallGroup` reads its state. */
function toolCallOf(activity: ConversationActivity): ChatToolCallItem {
  const duration = activity.durationMs !== null && activity.durationMs >= ACTIVITY_DURATION_FLOOR_MS
    ? formatActivityDuration(activity.durationMs)
    : undefined;
  return {
    key: activity.id,
    name: activity.verb,
    target: activity.target ?? undefined,
    status: activity.state === 'done' ? 'complete' : activity.state === 'failed' ? 'error' : 'running',
    duration,
    errorMessage: activity.detail ?? undefined,
    resultDetail: activity.detail === null
      ? undefined
      : <div data-nc-detail={activity.id}><Code>{activity.detail}</Code></div>,
  };
}

/** One action, one line. The failure reason is a second row inside the same `<p>`, which keeps `data-nc-state` on the line; a `flex-wrap` could not do it, since a flex line wraps before it shrinks. */
function ActivityLine({ activity, entry, live }: {
  activity: ConversationActivity;
  /** The run's carried key, stamped as `data-nc-entry` — a run of one is still a run (`useToolCallFocus`). */
  entry: string;
  live: boolean;
}) {
  const running = activity.state === 'running';
  const duration = !running && activity.durationMs !== null
    && activity.durationMs >= ACTIVITY_DURATION_FLOOR_MS
    ? formatActivityDuration(activity.durationMs)
    : null;
  return (
    <p
      className={`${styles.activity} ${activity.state === 'failed' ? styles.activityFailed : ''}`}
      data-nc-state={activity.state}
      data-nc-entry={entry}
    >
      <span className={styles.activityRow}>
        <span>{activity.verb}</span>
        {activity.target !== null
          && <span className={styles.activityTarget}>{activity.target}</span>}
        {activity.state === 'failed' && <span className={styles.activityFailure}>Failed</span>}
        {duration !== null && <span className={styles.activityDuration}>{duration}</span>}
        {running && live && <ActivityIndicator state="working" />}
      </span>
      {activity.detail !== null && (
        <span className={styles.activityDetail}>{activity.detail}</span>
      )}
    </p>
  );
}

/** `/new` in the composer: the panel column (and its `+`) is hidden while a drawer is open, so this is the only new-conversation door from inside a conversation. It runs the `+`'s own callback; `undefined` means no trigger at all. */
export const NEW_CONVERSATION_COMMAND = Object.freeze({
  id: 'new-conversation',
  /* What Astryx filters on, not what the row shows: `renderItem` reads nothing from the item. */
  label: 'New conversation',
});

/** Checked by shape: the `void` half of `onSend`'s signature is a return the caller does not make. */
function isThenable(value: unknown): value is Promise<SendOutcome> {
  return typeof (value as { then?: unknown } | null | undefined)?.then === 'function';
}

/** Astryx's ChatComposer; we own the value and the send callback so the kernel path stays a string. */
export function ChatComposer({
  onSend, onStop, onNewConversation, disabled = false, focusOnMount = false, draft: controlledDraft,
  footerActions, sendAdornment,
  drawer, headerActions, allowEmptyText = false,
}: {
  /** A caller with its own draft persistence returns `void`. */
  onSend: (text: string) => void | Promise<SendOutcome>;
  /** A route may retain unsent words across its recovery surfaces. */
  draft?: Readonly<{ text: string; onChange: Dispatch<SetStateAction<string>> }>;
  /** Interrupt the turn in flight; its presence turns Send into Stop. No `stopping` guard: `ChatSendButton` is unconditionally enabled while Stop is shown, so withholding the callback only empties its `onClick` — the rule lives at the top of the router's `interrupt()`. */
  onStop?: () => void;
  /** The same callback the module head's `+` fires; absent where the `+` is absent, which is what keeps the `/` menu from existing. */
  onNewConversation?: () => void;
  disabled?: boolean;
  /** Put the caret in the field as this composer mounts. Read once, at mount; the composer has no `key` on the router's path, so raising the flag again on a mounted composer does nothing — one mount per intent is the caller's job. */
  focusOnMount?: boolean;
  /** Per-conversation controls beside the field: a pass-through to Astryx's `footerActions` slot, because the controls need router state and `features/**` may not import `app/**`. */
  footerActions?: ReactNode;
  /** Something to stand immediately before Send, in the `sendActions` slot; rendered before the send-door button, not instead of it. */
  sendAdornment?: ReactNode;
  /** The composer's two vendor slots, passed as nodes: `features-no-cross-domain` forbids importing `features/planner`. */
  drawer?: ReactNode;
  headerActions?: ReactNode;
  /** Whether a send with no words is a real send (an attached image); a prop because the drawer is an opaque node. */
  allowEmptyText?: boolean;
}) {
  const [localDraft, setLocalDraft] = useState('');
  const draft = controlledDraft?.text ?? localDraft;
  const setDraft = controlledDraft?.onChange ?? setLocalDraft;
  const stopShown = onStop != null;

  /* Read through a ref so `triggers` can be a stable array: `useTriggerMenu` compares the active trigger by identity on every input event. */
  const newConversationRef = useRef(onNewConversation);
  newConversationRef.current = onNewConversation;

  const rootRef = useRef<HTMLDivElement>(null);
  const [sendCount, setSendCount] = useState(0);
  const wantsFieldFocus = useRef(focusOnMount);
  /** The element this component last put focus on; `null` while no restore is in flight. */
  const parkedFocus = useRef<Element | null>(null);

  /* Put the caret back after a send, as a standing request: `disabled` goes true inside the very click that sends, Astryx turns the field `contenteditable="false"`, and Chromium hands the focus to `<body>`. While the field refuses focus it is parked on the composer's own box, and the restore continues only while focus is exactly where it was parked. */
  useEffect(() => {
    if (!wantsFieldFocus.current) return;
    const root = rootRef.current;
    if (root === null) return;
    const parked = parkedFocus.current;
    if (parked !== null && document.activeElement !== parked) {
      wantsFieldFocus.current = false;
      parkedFocus.current = null;
      return;
    }
    const messageField = root.querySelector<HTMLElement>('[contenteditable="true"], textarea');
    messageField?.focus();
    if (messageField !== null && document.activeElement === messageField) {
      wantsFieldFocus.current = false;
      parkedFocus.current = null;
      return;
    }
    if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
    parkedFocus.current = document.activeElement;
  }, [sendCount, disabled]);

  const triggers = useMemo<ChatComposerTrigger[]>(() => [{
    character: '/',
    searchSource: createStaticSource([NEW_CONVERSATION_COMMAND]),
    menuLabel: 'Commands',
    emptySearchResultsText: 'No command by that name',
    /* `item.label` is deliberately not rendered. */
    renderItem: () => (
      <span className={styles.commandItem}>
        {/* The `+`'s own glyph: this is the same action. No label — `Icon` is `aria-hidden` and the row's name comes from the item's `label`. */}
        <Icon name="plus" size="sm" />
        <span className={styles.commandName}>new</span>
        <span className={styles.commandHint}>This one stays in the list</span>
      </span>
    ),
    /* A command is run, not inserted: returning `''` leaves the composer clear, and Astryx has already deleted the typed `/new`. */
    onSelect: () => {
      newConversationRef.current?.();
      return '';
    },
  }], []);

  /** Hand one message to the caller. `stopShown` is not a reason to refuse: `POST /planner/input` queues the text behind the turn in flight. `disabled` is the router's "last POST has not settled". */
  const submit = (value: string) => {
    const text = value.trim();
    /* `allowEmptyText` is the caller saying the message carries an image. */
    if ((text === '' && !allowEmptyText) || disabled) return;
    const outcome = onSend(text);
    setDraft('');
    /* Cleared optimistically, put back only for the outcome that says the server stored nothing, and only into an empty field. Excluded: `unresolved` (no idempotency key, so a second delivery is one Enter away), `abandoned` (another conversation) and `not-sent` (must not take the field from an earlier send still waiting). */
    if (isThenable(outcome)) {
      void outcome.then((result) => {
        if (result !== 'refused') return;
        setDraft((current) => current === '' ? text : current);
      });
    }
    /* The caret goes back from the effect above: `onSend` may already have queued the `disabled` that takes the field away. */
    wantsFieldFocus.current = true;
    /* A fresh request: the effect's first run must not compare against an earlier perch. */
    parkedFocus.current = null;
    setSendCount((count) => count + 1);
  };

  /* `Send image`: `ChatSendButton` takes its availability from `canSend`, false on an empty draft, and an image-only message is an empty draft. Shown only while the vendor's own button is unavailable. */
  const sendDoor = allowEmptyText && draft.trim() === '' && !stopShown ? (
    <button
      type="button"
      className={styles.queueSend}
      data-nc-send-attachment=""
      disabled={disabled}
      onClick={() => { submit(draft); }}
    >Send image</button>
  ) : undefined;

  return (
    <div
      ref={rootRef}
      className={styles.composer}
      data-nc-composer=""
      /* Programmatic focus only: the perch the send effect parks on so focus taken off a disabling Send is not on `<body>`. */
      tabIndex={-1}
      /* Named, because focus is parked here during a send and a screen reader announces it; `group` rather than a landmark, and a name that builds on the field's `Message` rather than repeating it. */
      role="group"
      aria-label="Message composer"
      onKeyDownCapture={(event) => {
        /* Astryx clears its input after Enter even when its parent refuses
           submission. Keep unsent words while disabled, and
           let IME Enter accept its candidate without submitting. */
        if (event.key !== 'Enter' || event.shiftKey) return;
        /* Only Enter pressed in the field is a send: this handler captures on the root, and the `drawer` slot puts buttons under it. */
        if (!(event.target instanceof Element)
          || event.target.closest('[contenteditable], textarea, input') === null) return;
        if (event.nativeEvent.isComposing) {
          event.stopPropagation();
          return;
        }
        if (disabled) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        /* Astryx's own `handleSubmit` refuses an empty draft (`if (!value.trim()) return;`), so an image-only send has to go through here. */
        if (allowEmptyText && draft.trim() === '') {
          event.preventDefault();
          event.stopPropagation();
          submit(draft);
        }
      }}
    >
      <AstryxChatComposer
        density="compact"
        value={draft}
        onChange={setDraft}
        placeholder="Say something"
        isDisabled={disabled}
        isStopShown={stopShown}
        footerActions={footerActions}
        /* Handed over whole — "one interrupt at a time" is the router's rule. */
        onStop={onStop}
        /* Astryx's own `handleSubmit` refuses only an empty draft and `isDisabled`, never `isStopShown`. */
        onSubmit={submit}
        {...(drawer === undefined ? {} : { drawer })}
        {...(headerActions === undefined ? {} : { headerActions })}
        sendActions={sendDoor === undefined && sendAdornment === undefined ? undefined : (
          <>{sendAdornment}{sendDoor}</>
        )}
        input={(
          <ChatComposerInput
            label="Message"
            placeholder="Say something"
            /* No triggers where there is no command: otherwise the field becomes an `aria-expanded="false"` combobox that can never expand. */
            {...(onNewConversation === undefined ? {} : { triggers })}
          />
        )}
        /* Send's availability is Astryx's own (`canSend`). Astryx renders `aria-disabled` only with a `tooltip`, which `ChatSendButton` does not take, so this is a native `disabled` that drops focus to `<body>` — the focus effect above moves it back into the field first. */
        sendButton={<ChatSendButton />}
      />
    </div>
  );
}

/** The strip: an `alert` region welded to the top edge of the composer well, rendered above `<ChatComposer>`. */
export function ChatFooterNotice({ children, tone = 'error' }: { children: ReactNode; tone?: 'error' | 'neutral' }) {
  return <div role={tone === 'neutral' ? 'status' : 'alert'}
    className={`${styles.footerNotice} ${tone === 'neutral' ? styles.footerNoticeNeutral : ''}`}>{children}</div>;
}

/** What went wrong, at caption rank; the strip around it carries the fill. */
export function ChatFooterError({ message }: { message: string }) {
  return <span className={styles.footerError}>{message}</span>;
}

/** The way out of that error, inline in the strip; `tertiary` so it does not compete with Send. */
export function ChatFooterRemedy({ disabled = false, onClick, children }: {
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button type="button" data-nc-action="tertiary" disabled={disabled} onClick={onClick}>
      {children}
    </button>
  );
}

/** A wall clock, not a relative time: the separator exists to say *when*, and
 *  "3h" is only useful when you already know when now is. */
function clockTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
}
