import { markdownExcerpt } from '../../../../../core/markdown/public.ts';
// The conversation itself: a transcript and the box you write into. The unit is the
// exchange — one thing you said and everything that came back.

import { useEffect, useLayoutEffect, useMemo, useRef, type ReactNode, type Dispatch, type SetStateAction } from 'react';
import { createPortal } from 'react-dom';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
  ChatLayoutScrollButton,
  type ChatComposerTrigger,
  type ChatToolCallItem,
} from '@astryxdesign/core/Chat';
import { Badge } from '@astryxdesign/core/Badge';
import { Code } from '@astryxdesign/core/Code';
import { createStaticSource } from '@astryxdesign/core/Typeahead';
import { Button } from '@astryxdesign/core/Button';
import { useIcon } from '@astryxdesign/core/Icon';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { EdgeNavigator } from '../../../ui/edge-navigation/public.tsx';
import { observeResize } from '../../../ui/edge-navigation/resize.ts';
import { createScrollFollower } from '../../../ui/drawer/follow-scroll.ts';
import { drawerSeamAround } from '../../../ui/drawer/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { triggerMenuKeyRoute } from '../../../ui/trigger-menu-keys/public.ts';
import { useState } from '../../../ui/state/public.ts';

import { activityLabelOf, cardActivityOf, type CardActivity } from '../../../../../core/domain/activity.ts';
import { sentMentionParts } from '../../../../../core/domain/mentions.ts';
import { foldQuietSyncs } from '../../../../../core/domain/conversation-quiet-sync.ts';
import {
  isQueuedConversationTurn, opensAfterGap, opensExchange,
  type Conversation, type ConversationTurn, type ConversationActivity, type ConversationTurnOutcome,
  type TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import { QuietSyncFold } from './quiet-sync.tsx';
import { Reply, type ReplyImageFiles } from './reply.tsx';
import styles from './thread.module.css';
import { sideQuestion } from '../../../../../core/domain/side-conversation.ts';
import { currentResponseMessage, latestUserMessage } from '../../../../../core/domain/conversation-actions.ts';
import { CurrentStatusNotice } from './outcome-notice.tsx';
import { SizeMotion } from '../../../ui/motion/size.tsx';
import { editedTurnMessageIds } from '../../../../../core/domain/conversation-composer.ts';
import type { ConversationStopFeedback } from '../../../../../core/domain/conversation-stop.ts';
import type { RunningTurnAnchor } from '../../../../../core/domain/conversation-meta.ts';
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
  /** The runtime reason is separate from the persisted terminal transcript. */
  stalledReason?: string | null;
  stopFeedback?: ConversationStopFeedback | null;
  /** The caller declares composer availability; a transcript outcome cannot authorize sends. */
  canContinue: boolean;
  copyText?: (text: string) => Promise<void>;
  regenerateMessage?: (message: ConversationTurn) => Promise<void>;
  /** Put the message of the turn that ends at `outcome` in the composer, in edit mode; the caller owns what Send then does. */
  editMessage?: (outcome: ConversationTurnOutcome) => void;
  /** The outcome of the turn being edited: its messages stay on screen, marked. */
  editing?: string | null;
  /** A failed send that replaces that turn: its message is drawn as the replacement, not as a second copy (#2068). */
  replacement?: string | null;
  /** Where the running turn's clock starts, from the run response; `null` draws `Running` with no number. */
  runningAnchor?: RunningTurnAnchor | null;
  /** Local reply images use the open conversation's workspace, never the browser's URL base. */
  imageFiles?: ReplyImageFiles | null;
}>;

export function ChatThread({ conversation, turns, pending = false, cards, stalled, stalledReason, stopFeedback = null, canContinue, copyText, regenerateMessage, editMessage, editing = null, replacement = null, runningAnchor = null, imageFiles = null }: ChatThreadProps) {
  /* The live mark is the sender's pending send or the kernel's verdict — never `conversation.state`, which sits at `turn_pending`/`running` long after a turn ended. The local wedge outranks both. */
  const live = !stalled && (pending || cardActivityOf({ cards }, conversation.id) === 'working');
  const lastTurn = turns[turns.length - 1];
  const currentOutcome = lastTurn?.author === 'turn' ? lastTurn : null;
  const copyTarget = currentResponseMessage(turns, currentOutcome !== null && !live && !stalled && stopFeedback === null);
  const copyAction = copyText === undefined || copyTarget === null ? null
    : { id: `${conversation.id}:${copyTarget.id}`, text: copyTarget.text, run: () => copyText(copyTarget.text) };
  const regenerateTarget = latestUserMessage(turns);
  const regenerateAction = regenerateMessage === undefined || regenerateTarget === null || live || stalled || currentOutcome === null
    ? null : { id: `${conversation.id}:${regenerateTarget.id}`, run: () => regenerateMessage(regenerateTarget) };
  /* Only the latest turn, and only one the reader started: its outcome names the turn the server removes. */
  const editAction = editMessage === undefined || regenerateTarget === null || live || stalled || currentOutcome === null
    || currentOutcome.turnId === '' ? null
    : { id: `${conversation.id}:${currentOutcome.turnId}`, run: () => {
      // Enter immediately. Presentation must not delay edit state or the caret.
      editMessage(currentOutcome);
    } };
  const currentMeta = <CurrentStatusNotice outcome={currentOutcome} canContinue={canContinue} live={live}
    stalled={stalled} stalledReason={stalledReason ?? null} feedback={stopFeedback} copyAction={copyAction} editAction={editAction} regenerateAction={regenerateAction} runningAnchor={runningAnchor} />;
  const endRef = useRef<HTMLDivElement | null>(null);
  /** The box every marker lookup starts from. Not `.thread` itself: the stylesheet's `> * + *` rules space that element's children. */
  const frameRef = useRef<HTMLDivElement | null>(null);
  const exchanges = useMemo(() => exchangesOf(turns), [turns]);
  const edited = useMemo(() => editing === null ? new Set<string>() : editedTurnMessageIds(turns, editing), [editing, turns]);
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
    transcriptGroups.filter(({ entry }) => entry.author !== 'turn'),
    (activities) => activities.map(toolCallOf),
    (key) => groupUi.get(key) ?? untouchedToolCallGroup(),
  );
  /* Whether the end of the transcript already says "working", so the placeholder mark stays down: a run's *visible* rows count — the latest call when closed, every call when open. */
  const tail = transcriptGroups[transcriptGroups.length - 1];
  const tailQuiet = tail === undefined ? undefined : quietBlocks.get(tail.entry.id);
  const tailCarriesLiveMark = tail === undefined ? false
    : tailQuiet !== undefined ? tailQuiet.entries.some((entry) => entry === lastTurn)
    : tail.activities === null ? tail.entry.author === 'agent'
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
  const [scrolledUp, setScrolledUp] = useState(false);
  const [scrollFollower] = useState(() => createScrollFollower({
    bottomSlack: FOLLOW_BOTTOM_SLACK_PX, onAwayChange: setScrolledUp,
  }));
  /** The turn at the end of the transcript as of this render — what the follow
   *  effect below both depends on and decides by. */
  const newestId = lastTurn?.id;
  /** How much of the newest reply there is: a streamed reply grows in place under one id. */
  const newestLength = lastTurn?.author === 'agent' ? lastTurn.text.length : 0;
  /** The newest turn as of the last run of the follow effect, so *Load earlier* is not mistaken for an arrival. Updated on every run, including runs that decline to scroll. */
  const followedTo = useRef<string | undefined>(undefined);
  const followedLength = useRef(0);
  const followedNotice = useRef<string | null>(null);
  const noticeKind = stalled ? 'paused' : stopFeedback?.kind ?? null;

  // Attach once per pane/conversation. Input intent survives streaming renders.
  useEffect(() => {
    const content = frameRef.current;
    const scroller = content?.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (content == null || scroller == null) return;
    return scrollFollower.attach(scroller, content);
  }, [scrollFollower, hasTranscript, conversation.id]);

  // Tail arrivals are distinct from older history being prepended. Content
  // resize also follows late image loads, rewraps and disclosure growth.
  useEffect(() => {
    const arrived = newestId !== followedTo.current || newestLength !== followedLength.current
      || noticeKind !== followedNotice.current;
    followedNotice.current = noticeKind;
    followedTo.current = newestId;
    followedLength.current = newestLength;
    if (arrived) scrollFollower.followGrowth();
  }, [scrollFollower, newestId, newestLength, noticeKind]);

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

  /* A run of calls, one or more, keyed by `key`: Astryx draws one call inline and two or more as a group. `runLive` is the live tail's alone — a run the conversation has moved past must not look like it resumed. */
  const renderRun = (activities: readonly ConversationActivity[], key: string, runLive: boolean): ReactNode => (
    <ToolCallGroup
      /* Carried by membership, not read off any one call, so the run keeps its element as calls are appended, prepended or dropped. */
      key={key}
      entry={key}
      calls={activities.map(toolCallOf)}
      ui={groupUi.get(key) ?? untouchedToolCallGroup()}
      onExpandedChange={(expanded) => updateGroupUi(key, (ui) => ({ ...ui, expanded }))}
      onDetailOpenChange={(callKey, open) => updateGroupUi(key, (ui) => withDetailOpen(ui, callKey, open))}
      live={runLive}
    />
  );

  /* One entry, in the position `turns` gives it; `index` is what the exchange, gap and live-mark rules are stated over. */
  const renderEntry = (turn: TranscriptEntry, key = turn.id, showLive = live): ReactNode => {
    const index = indexOf.get(turn) ?? -1;
    const last = index === turns.length - 1;
    if (turn.author === 'activity') return renderRun([turn], key, showLive && last);
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
    // Outcomes retain their grouping boundary; only the current metadata row paints status.
    if (turn.author === 'turn') return null;
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
            {/* The caption is outside the message; persisted references recover their display pills. */}
            <p
              className={styles.said}
              data-nc-turn="you"
              {...(isQueuedConversationTurn(turn) ? { 'data-nc-queued': '' } : {})}
              {...(edited.has(turn.id) ? { 'data-nc-editing': '' } : {})}
              {...(turn.id === replacement ? { 'data-nc-replacement': '' } : {})}
            >{sentMentionParts(turn.text).map((part, index) => part.label === null ? part.text : (
              <span key={index} data-nc-sent-mention="" title={part.text}>
                <Badge className={styles.mentionPill}
                  label={<span className={styles.mentionLabel}>{part.label}</span>} />
              </span>
            ))}</p>
            {/* `alt=""` and `aria-hidden`: the transcript has no description of the image to offer, and the count is said once in text above. */}
            {(turn.attachments ?? []).length > 0 && (
              <ul className={styles.attachments} data-nc-turn-attachments="" {...(edited.has(turn.id) ? { 'data-nc-editing': '' } : {})}>
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
            {turn.id === replacement && <p className={styles.queuedNote}>Replaces the marked message above</p>}
          </>
        ) : (
          <div className={styles.reply} data-nc-turn="agent">
            <Reply text={turn.text} imageFiles={imageFiles} />
            {showLive && last && <ActivityIndicator state="working" motion="thinking" />}
          </div>
        )}
      </div>
    );
  };

  if (turns.length === 0 && noticeKind === null) {
    return (
      <div className={styles.empty} data-nc-thread-empty="">
        <p className={styles.emptyLead}>{live ? 'The agent is working.' : 'Nothing said yet.'}</p>
        <p className={styles.emptyHint}>{live ? 'Messages will appear here.' : 'Write below and it starts here.'}</p>
        {live && <ActivityIndicator state="working" motion="thinking" />}
        {currentMeta}
      </div>
    );
  }

  return (
    <div className={styles.threadFrame} ref={frameRef}>
      {railShown && railSeam !== null && createPortal(
        <EdgeNavigator
          className={styles.edgeNavigation}
          label="Jump to an exchange"
          items={exchanges.map((item, index) => ({ id: item.id, title: item.text.trim() === '' ? `Exchange ${index + 1}` : item.text, excerpt: item.excerpt,
            label: `Jump to exchange ${index + 1}${railLabel(item.text) === '' ? '' : `: ${railLabel(item.text)}`}`,
          }))}
          activeId={active}
          onSelect={(id) => {
            /* The lookup comes first: a marker that is not there leaves the mark untouched. */
            const target = exchangePosition(frameRef.current, id);
            if (target === null) return;
            scrollFollower.navigate(target);
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
          if (activities !== null) return renderRun(activities, key, live && activities.at(-1) === lastTurn);
          return renderEntry(turn, key);
        })}
        {/* A reply that has not arrived yet still gets a place to arrive in; the placeholder keeps the one mark visible unless the tail carries it. */}
        {live && !tailCarriesLiveMark && (
          <p className={styles.reply}><ActivityIndicator state="working" motion="thinking" /></p>
        )}
        {/* The drawer's one accessible "in motion" fact: every indicator is decorative. It sits after the placeholder because the stylesheet spaces `.thread`'s children by adjacency (`.exchange + *`). */}
        {live && <VisuallyHidden>{activityLabelOf('working')}</VisuallyHidden>}
        {currentMeta}
        <div ref={endRef} aria-hidden="true" />
      </div>
      {scrolledUp && (
        <div className={styles.scrollDock}>
          <div className={styles.scrollDockContent} data-nc-chat-scroll-dock="">
            <div className={styles.scrollBlur} aria-hidden="true" />
            <ChatLayoutScrollButton isVisible className={styles.scrollButton} onClick={scrollFollower.followToEnd} />
          </div>
        </div>
      )}
    </div>
  );

}

/** One thing you said and everything that came back — as far as the rail needs
 *  it: something to point at, and the words to call it by. */
type Exchange = Readonly<{ id: string; text: string; excerpt: string }>;

/** How far below the pane's top a marker may still count as "scrolled past": absorbs the subpixel gap between the scroll asked for and the one the engine performs, at both ends. */
const ACTIVE_MARKER_SLACK_PX = 4;

/** How far from the bottom still counts as reading the newest turn: 2.6 lines of the reply's 24.75px line box. */
const FOLLOW_BOTTOM_SLACK_PX = 64;





function exchangesOf(turns: readonly TranscriptEntry[]): readonly Exchange[] {
  const found: Exchange[] = [];
  turns.forEach((turn, index) => {
    /* `opensExchange` already implies `author === 'you'`; the narrowing below is
       for the type checker, which cannot read that from the domain function. */
    if (!opensExchange(turns, index) || turn.author !== 'you') return;
    const following = turns.slice(index + 1);
    const nextPrompt = following.findIndex(entry => entry.author === 'you');
    const exchange = nextPrompt < 0 ? following : following.slice(0, nextPrompt);
    const reply = exchange.find(entry => entry.author === 'agent');
    found.push({ id: turn.id, text: turn.text, excerpt: markdownExcerpt(reply !== undefined && reply.author === 'agent' ? reply.text : '') });
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

/** Resolve before releasing follow intent: an absent exchange is a no-op. */
function exchangePosition(frame: HTMLElement | null, id: string): (() => void) | null {
  if (frame === null) return null;
  const marker = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')]
    .find((candidate) => candidate.dataset.ncExchange === id);
  if (marker === undefined) return null;
  const scroller = marker.closest<HTMLElement>('[data-nc-drawer-scroll]');
  if (scroller === null) return null;
  return () => { scroller.scrollTop += marker.getBoundingClientRect().top - scroller.getBoundingClientRect().top; };
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

/** Every run of activities renders through the vendor's tool calls: one inline, two or more grouped. `key` is the activity id (stable from started to completed); `resultDetail` is wrapped in `data-nc-detail`, which the vendor mounts only while open — that is how `ToolCallGroup` reads its state. The vendor prints `duration` on completed calls only. */
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

/** `/new` in the composer: the panel column (and its `+`) is hidden while a drawer is open, so this is the only new-conversation door from inside a conversation. It runs the `+`'s own callback; `undefined` means no trigger at all. */
export const NEW_CONVERSATION_COMMAND = Object.freeze({
  id: 'new-conversation',
  /* What Astryx filters on, not what the row shows: `renderItem` reads nothing from the item. */
  label: 'New conversation',
});

export const SIDE_CONVERSATION_COMMAND = Object.freeze({ id: 'side-conversation', label: 'Side conversation' });

/** Astryx's ChatComposer; we own the value and the send callback so the kernel path stays a string. */
export function ChatComposer({
  onSend, onStop, onNewConversation, onSideConversation, showSideCommand = true, disabled = false, sendWaiting = false, editing, focusOnMount = false, focusRequest = 0,
  draft: controlledDraft, footerActions, sendAdornment,
  drawer, headerActions, allowEmptyText = false, mentionTrigger,
}: {
  /** Hand over the words, or return `false` when they were not taken and stay in the field. Words of a send taken
   * and later given back return through the caller's `draft`; the composer has no second way to restore them. */
  onSend: (text: string) => boolean | void;
  /** A route may retain unsent words across its recovery surfaces. */
  draft?: Readonly<{ text: string; onChange: Dispatch<SetStateAction<string>> }>;
  /** Interrupt the turn in flight; its presence turns Send into Stop. No `stopping` guard: `ChatSendButton` is unconditionally enabled while Stop is shown, so withholding the callback only empties its `onClick` — the rule lives at the top of the router's `interrupt()`. */
  onStop?: () => void;
  /** The same callback the module head's `+` fires; absent where the `+` is absent, which is what keeps the `/` menu from existing. */
  onNewConversation?: () => void;
  /** Starts a separate discussion using a frozen context excerpt. */
  onSideConversation?: (question: string) => void | boolean;
  /** Hide this command on surfaces that cannot open a second card. */
  showSideCommand?: boolean;
  disabled?: boolean;
  /** A send was taken and waits for its answer (an Edit's replace is still out): Send turns into a spinner that takes no press. */
  sendWaiting?: boolean;
  /** Edit mode: a bar names the message being replaced, ✕ or Esc leaves the mode, and Send is "Replace message". */
  editing?: Readonly<{ preview: string; onCancel: () => void }>;
  /** Put the caret in the field as this composer mounts. Read once, at mount; the composer has no `key` on the router's path, so raising the flag again on a mounted composer does nothing — one mount per intent is the caller's job. */
  focusOnMount?: boolean;
  /** Each new value asks once for the caret, on a mounted composer (an Edit's refill); the same wait as after a send. */
  focusRequest?: number;
  /** Per-conversation controls beside the field: a pass-through to Astryx's `footerActions` slot, because the controls need router state and `features/**` may not import `app/**`. */
  footerActions?: ReactNode;
  /** Something to stand immediately before Send, in the `sendActions` slot; rendered before the send-door button, not instead of it. */
  sendAdornment?: ReactNode;
  /** The composer's two vendor slots, passed as nodes: `features-no-cross-domain` forbids importing `features/planner`. */
  drawer?: ReactNode;
  headerActions?: ReactNode;
  /** Whether a send with no words is a real send (an attached image); a prop because the drawer is an opaque node. */
  allowEmptyText?: boolean;
  /** The `@` menu (`useMentionTrigger`), present only where a Planner reads what is sent; the caller keeps it stable. */
  mentionTrigger?: ChatComposerTrigger;
}) {
  const [localDraft, setLocalDraft] = useState('');
  const draft = controlledDraft?.text ?? localDraft;
  const sendIcon = useIcon('arrowUp');
  const setDraft = controlledDraft?.onChange ?? setLocalDraft;
  const stopShown = onStop != null;

  /* Read through a ref so `triggers` can be a stable array: `useTriggerMenu` compares the active trigger by identity on every input event. */
  const newConversationRef = useRef(onNewConversation);
  newConversationRef.current = onNewConversation;
  const sideConversationRef = useRef(onSideConversation);
  sideConversationRef.current = onSideConversation;

  const rootRef = useRef<HTMLDivElement>(null);
  const [sendCount, setSendCount] = useState(0);
  const wantsFieldFocus = useRef(focusOnMount);
  /** The element this component last put focus on; `null` while no restore is in flight. */
  const parkedFocus = useRef<Element | null>(null);
  const answeredFocusRequest = useRef(focusRequest);

  /* Put the caret back after a send, as a standing request: `disabled` goes true inside the very click that sends, Astryx turns the field `contenteditable="false"`, and Chromium hands the focus to `<body>`. While the field refuses focus it is parked on the composer's own box, and the restore continues only while focus is exactly where it was parked. */
  useEffect(() => {
    if (answeredFocusRequest.current !== focusRequest) {
      answeredFocusRequest.current = focusRequest;
      wantsFieldFocus.current = true;
      parkedFocus.current = null;
    }
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
    messageField?.focus({ preventScroll: true });
    if (messageField !== null && document.activeElement === messageField) {
      wantsFieldFocus.current = false;
      parkedFocus.current = null;
      return;
    }
    if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
    parkedFocus.current = document.activeElement;
  }, [sendCount, disabled, focusRequest]);

  const hasNewCommand = onNewConversation !== undefined;
  const hasSideCommand = onSideConversation !== undefined && showSideCommand;
  const commandTrigger = useMemo<ChatComposerTrigger>(() => ({
    character: '/',
    searchSource: createStaticSource([
      ...(hasNewCommand ? [NEW_CONVERSATION_COMMAND] : []),
      ...(hasSideCommand ? [SIDE_CONVERSATION_COMMAND] : []),
    ]),
    menuLabel: 'Commands',
    emptySearchResultsText: 'No command by that name',
    /* `item.label` is deliberately not rendered. */
    renderItem: (item) => (
      <span className={styles.commandItem}>
        {/* The `+`'s own glyph: this is the same action. No label — `Icon` is `aria-hidden` and the row's name comes from the item's `label`. */}
        <Icon name="plus" size="sm" />
        <span className={styles.commandName}>{item.id === SIDE_CONVERSATION_COMMAND.id ? 'side' : 'new'}</span>
        <span className={styles.commandHint}>{item.id === SIDE_CONVERSATION_COMMAND.id ? 'Discuss with a text snapshot' : 'This one stays in the list'}</span>
      </span>
    ),
    /* A command is run, not inserted: returning `''` leaves the composer clear, and Astryx has already deleted the typed `/new`. */
    onSelect: (item) => {
      if (item.id === SIDE_CONVERSATION_COMMAND.id) sideConversationRef.current?.('');
      else newConversationRef.current?.();
      return '';
    },
  }), [hasNewCommand, hasSideCommand]);
  const hasCommands = onNewConversation !== undefined || onSideConversation !== undefined;
  /* One array per combination: a new array per render would make `useTriggerMenu` drop an open menu. */
  const triggers = useMemo<ChatComposerTrigger[]>(() => [
    ...(hasCommands ? [commandTrigger] : []),
    ...(mentionTrigger === undefined ? [] : [mentionTrigger]),
  ], [hasCommands, commandTrigger, mentionTrigger]);

  /** Hand one message to the caller. `stopShown` is not a reason to refuse: `POST /planner/input` queues the text behind the turn in flight. `disabled` is the router's "last POST has not settled". */
  const submit = (value: string) => {
    const text = value.trim();
    /* `allowEmptyText` is the caller saying the message carries an image. */
    if ((text === '' && !allowEmptyText) || disabled) return;
    const question = onSideConversation === undefined ? null : sideQuestion(text);
    if (question !== null) {
      if (onSideConversation?.(question) === false) {
        // Astryx clears after calling onSend; restore after its clear, like a refused send.
        void Promise.resolve().then(() => setDraft((current) => current === '' ? text : current));
        return;
      }
      setDraft('');
      return;
    }
    const taken = onSend(text);
    setDraft('');
    /* Astryx clears after calling `onSend`: put an untaken message back after that clear, into an empty field. */
    if (taken === false) void Promise.resolve().then(() => setDraft((current) => current === '' ? text : current));
    /* The caret goes back from the effect above: `onSend` may already have queued the `disabled` that takes the field away. */
    wantsFieldFocus.current = true;
    /* A fresh request: the effect's first run must not compare against an earlier perch. */
    parkedFocus.current = null;
    setSendCount((count) => count + 1);
  };

  /* `Send image`: `ChatSendButton` takes its availability from `canSend`, false on an empty draft, and an image-only message is an empty draft. Shown only while the vendor's own button is unavailable. */
  const sendDoor = allowEmptyText && draft.trim() === '' && !stopShown && !sendWaiting && editing === undefined ? (
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
        if (event.key !== 'Enter' && event.key !== 'Tab' && event.key !== 'Escape') return;
        /* Only Enter pressed in the field is a send: this handler captures on the root, and the `drawer` slot puts buttons under it. */
        const field = event.target instanceof Element ? event.target.closest('[contenteditable], textarea, input') : null;
        if (field === null) return;
        if (event.key === 'Escape') {
          /* Esc leaves edit mode, unless it is closing an open `/` or `@` menu; handled, so the drawer stays open. */
          if (editing === undefined || field.getAttribute('aria-expanded') === 'true' || event.nativeEvent.isComposing) return;
          event.preventDefault();
          event.stopPropagation();
          editing.onCancel();
          return;
        }
        const route = triggerMenuKeyRoute(event.nativeEvent, field);
        if (route === 'composing' || route === 'swallow') {
          if (route === 'swallow') event.preventDefault();
          event.stopPropagation();
          return;
        }
        if (route === 'menu' || event.key !== 'Enter' || event.shiftKey) return;
        if (disabled) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        /* A send over an open menu with nothing to pick: Astryx's submit empties the field but leaves the
           menu armed at the old offset, so the next pick throws. Its own Escape resets it first. Cancelable,
           so Astryx's `preventDefault` marks it handled and the drawer's Escape does not close the drawer;
           the router's Escape listener leaves an expanded combobox alone. */
        if (field.getAttribute('aria-expanded') === 'true') {
          field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
        }
        /* Astryx's own `handleSubmit` refuses an empty draft (`if (!value.trim()) return;`), so an image-only send has to go through here. */
        if (allowEmptyText && draft.trim() === '') {
          event.preventDefault();
          event.stopPropagation();
          submit(draft);
        }
      }}
    >
      <SizeMotion motionKey={editing !== undefined}>
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
          {...(editing === undefined && headerActions === undefined ? {} : { headerActions: editing === undefined ? headerActions : (
            <div className={styles.editBar} data-nc-edit-bar="">
              <span className={styles.editBarLabel}>Editing message</span>
              <span className={styles.editBarPreview}>{editing.preview}</span>
              <button type="button" className={styles.editBarCancel} aria-label="Cancel edit" title="Cancel edit"
                disabled={sendWaiting} onClick={editing.onCancel}><Icon name="close" size="sm" /></button>
            </div>
          ) })}
          sendActions={sendDoor === undefined && sendAdornment === undefined ? undefined : (
            <>{sendAdornment}{sendDoor}</>
          )}
          input={(
            <ChatComposerInput
              label="Message"
              placeholder="Say something"
              /* No triggers where there is neither a command nor a mention: otherwise the field becomes an `aria-expanded="false"` combobox that can never expand. */
              {...(triggers.length === 0 ? {} : { triggers })}
              /* The `@` source waits out keystrokes itself (`MENTION_SEARCH_DELAY_MS` says why Astryx's own delay must be off); the `/` source is synchronous and never delayed. */
              debounceMs={0}
            />
          )}
          /* Send's availability is Astryx's own (`canSend`). Astryx renders `aria-disabled` only with a `tooltip`, which `ChatSendButton` does not take, so this is a native `disabled` that drops focus to `<body>` — the focus effect above moves it back into the field first. */
          /* `ChatSendButton` has no busy state and a fixed label; in those two states this is its button under the name it then has. */
          sendButton={sendWaiting ? <Button label="Sending…" variant="primary" isIconOnly isLoading icon={sendIcon} className={styles.sendOwn} />
            : editing === undefined ? <ChatSendButton />
              : <Button label="Replace message" variant="primary" isIconOnly icon={sendIcon} className={styles.sendOwn}
                isDisabled={disabled || (draft.trim() === '' && !allowEmptyText)} onClick={() => { submit(draft); }} />}
        />
      </SizeMotion>
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
