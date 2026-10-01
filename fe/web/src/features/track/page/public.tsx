import { TrackTitle } from '../title/public.tsx';
// The `/track/$trackId` surface. Presentational: every mutation and navigation leaves through a callback,
// and there is no `<a href>` anywhere on this page.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { DropdownMenu as AstryxDropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { getIcon as getAstryxIcon } from '@astryxdesign/core/Icon';
import { Markdown } from '@astryxdesign/core/Markdown';
import { MoreMenu as AstryxMoreMenu } from '@astryxdesign/core/MoreMenu';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { useCallback, useEffect, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useCompactViewport } from '../../../ui/viewport/public.ts';

import { notificationPlainText } from '../../../../../core/domain/activity.ts';
import type { ReportOutlineItem, ReportTaskRow } from '../../../../../core/domain/report.ts';
import {
  UNTITLED_TRACK_LABEL, trackActivityState, trackDisplayTitle, type CardWire, type Track,
} from '../../../../../core/domain/track.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { DELETE_TRACK_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle, type EditableTitleProps } from '../../../ui/editable-title/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { MobileList, MobileListItem, MobileListPage } from '../../../ui/mobile-list/public.tsx';
import { MobileHeader } from '../../../ui/mobile-header/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import {
  OperationFeedback, useDeleteConfirm, useOperationFeedback,
} from '../../../ui/operation-feedback/public.tsx';
import { PanelCard, PanelModule } from '../../../ui/panel-card/public.tsx';
import { relativeTime } from '../row/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import type { RowModuleView, TrackPageView } from '../../../../../core/view/panel.ts';
import { TrackClosedBadge } from '../closed-badge/public.tsx';
import { makeDesktopPainter, paintDesktopPanel } from './desktop-painter.tsx';
import { makeMobilePainter, paintMobileModule } from './mobile-painter.tsx';
import { MobileTitleReadView } from './mobile-title-read-view.tsx';
import styles from './page.module.css';

/** The mobile drill-down pages: two that are not row modules, plus one per row module the view model names. The renderer special-cases exactly those two and sends every other member through `paintMobileModule`. */
type MobilePanelKind = 'outline' | RowModuleView['key'] | 'conversations';

/** One row of the Notifications aside — the route's projection of one item of `TrackActivity.attentionItems`. */
export type TrackInputNotification = Readonly<{
  /** The kernel's item key; the row's key. */
  key: string;
  /** `ask`: the Planner asks the user something; `planner-down`: the Planner stopped. */
  kind: 'ask' | 'planner-down';
  /** The kernel's words, shown verbatim: the Planner's question, or the reason it stopped. */
  text: string;
  atMs: number;
}>;

/** A row's meta line: what kind of thing is waiting, in plain words; only its dot carries colour. */
const NOTIFICATION_LABEL = Object.freeze({ ask: 'Needs your answer', 'planner-down': "Planner can't continue" } as const);

/** What clicking a row does: shown in the label's place while the row is hovered or focused, and the start of its accessible name. */
const NOTIFICATION_ACTION = Object.freeze({
  ask: Object.freeze({ hint: 'Answer in Planner', name: 'Answer the Planner' }),
  'planner-down': Object.freeze({ hint: 'Open Planner', name: 'Open the Planner' }),
} as const);

/** The start of a row's words, as plain text, for its accessible names; the whole text is in the Planner conversation. */
function notificationGist(text: string): string {
  const chars = [...notificationPlainText(text)];
  return chars.length > 80 ? `${chars.slice(0, 80).join('')}…` : chars.join('');
}

export type TrackPageProps = Readonly<{
  track: Track;
  cards: readonly CardWire[];
  /** The track's tasks, joined by `app/router` from the report's `task` blocks and the kernel's verdicts. */
  tasks: readonly ReportTaskRow[];
  /** The cards the board can draw, as `app/router` asks the registry. Required: a default here would be a second, silent verdict. It gates only whether a Task row's kind is an `open-card` control. */
  openableCards: ReadonlySet<string>;
  /** Report anchors rendered as a separate mobile list instead of a margin rail. */
  outlineItems?: readonly ReportOutlineItem[];
  /** The panel card's second module, composed by `app/router` (features/chat). */
  /** The report document, composed by `app/router` (features/report). */
  report?: ReactNode;
  /** `REFERENCED BY` — omitted entirely when nothing cites this track. */
  backlinks?: ReactNode;
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Everything the kernel says is addressed to the user on this track, projected by
   *  the route from the activity overlay's items (`attentionItems`). */
  inputNotifications?: readonly TrackInputNotification[];
  /** Opens the Planner's composer: what a click anywhere on a row does, an ask and planner down alike. */
  onReply?: () => void;
  /** Dismiss the row's item by its kernel key. The row stays until the overlay no longer lists it. */
  onDismiss?: (key: string) => Promise<void>;
  /** The clock the rows' relative times read; the current time when omitted. */
  nowMs?: number;
  /** The route's conversation drawer is open. Input notifications compact
   *  beside it instead of covering its composer. */
  conversationOpen?: boolean;
  /** Any active foreground drawer makes the painted mobile panel inaccessible. */
  mobilePanelObscured: boolean;
  /** Starts Chat from the mobile Report's dedicated floating action. */
  onStartConversation?: () => void;
  /** The Cards module head's `+`, composed by `app/router`. */
  cardsAction?: ReactNode;
  /** Browser-local file history, composed by the report feature through app. */
  recentFiles?: ReactNode;
  onOpenCard?: (cardId: string) => void;
  /** Supplying this reveals a delete on every row the kernel says is deletable. The caller owns the confirm: the board offers the identical gesture on the card's own head. */
  onDeleteCard?: (cardId: string) => void;
  /** Reveal a task's block in the document — the same landing the outline, a
   *  `neige://` link and a backlink all use. */
  onOpenTask?: (blockId: string) => void;
  onOpenOutline?: (blockId: string) => void;
  /** The card grid or transient file viewer, composed by `app/router` and positioned over the document column. */
  board?: ReactNode;
  onCloseBoard?: () => void;
  /** Which secondary panel the mobile report is showing, or `null`. The panel is a navigation destination, so its identity lives in the URL and `app/router` reads it. */
  panel?: MobilePanelKind | null;
  onOpenPanel?: (panel: MobilePanelKind) => void;
  onClosePanel?: () => void;
  mobileBackLabel?: string;
  onMobileBack?: () => void;
  /** Optional app-owned phone header slot; this feature keeps ownership of actions and focus. */
  mobileHeaderActionsHost?: HTMLElement | null;
  mobileHeaderTitleHost?: HTMLElement | null;
  mobileTitleReadView?: EditableTitleProps['readView'];
  /** The server's capabilities; at most one is true, so the menu offers Close or Reopen, never both. */
  canReopenTrack: boolean;
  canCloseTrack: boolean;
  onRenameTrack: (title: string) => void | Promise<void>;
  onReopenTrack: () => void | Promise<void>;
  onCloseTrack: () => void | Promise<void>;
  onDeleteTrack: (signal: AbortSignal) => void | Promise<void>;
}>;

/** The view model's module under `key`, by key rather than by index. Missing is an error rather than an empty page. */
function rowModule(view: TrackPageView, key: RowModuleView['key']): RowModuleView {
  const found = view.rowModules.find((module) => module.key === key);
  if (found === undefined) throw new Error(`the track page view has no ${key} module`);
  return found;
}

function taskInventorySummary(tasks: readonly ReportTaskRow[]): string | null {
  return tasks.length === 0 ? null : String(tasks.length);
}

export function TrackPage({
  track, cards, tasks, openableCards, outlineItems = [], report, backlinks, conversationList, conversationAction,
  onStartConversation, conversationOpen = false, mobilePanelObscured, inputNotifications = [], onReply, onDismiss, nowMs,
  cardsAction, recentFiles, onOpenCard, onDeleteCard, onOpenTask, onOpenOutline, board, onCloseBoard,
  panel = null, onOpenPanel, onClosePanel,
  mobileBackLabel = 'Pages', onMobileBack, mobileHeaderActionsHost = null, mobileHeaderTitleHost = null, mobileTitleReadView,
  canReopenTrack, canCloseTrack, onRenameTrack, onReopenTrack, onCloseTrack, onDeleteTrack,
}: TrackPageProps) {
  const compactViewport = useCompactViewport();
  const headerActionsHost = compactViewport ? mobileHeaderActionsHost : null;
  const [titleContainer] = useState(() => {
    const container = document.createElement('div');
    container.style.display = 'contents';
    return container;
  });
  useLayoutEffect(() => () => titleContainer.remove(), [titleContainer]);
  const desktopTitleSlotRef = useRef<HTMLDivElement | null>(null);
  const [mobileTitleReadHost] = useState(() => document.createElement('div'));
  const lastFocusedTitleRef = useRef<HTMLElement | null>(null);
  const titleReadControlRef = useRef<HTMLButtonElement | null>(null);
  const mobileTitleEditRef = useRef<(() => void) | null>(null);
  const [canEditMobileTitle, setCanEditMobileTitle] = useState(false);
  const registerMobileTitleEdit = useCallback((begin: (() => void) | null) => {
    mobileTitleEditRef.current = begin;
    setCanEditMobileTitle(begin !== null);
  }, []);
  const relocatingTitleRef = useRef(false);
  const titleInHeader = compactViewport && mobileHeaderTitleHost !== null;
  useLayoutEffect(() => {
    const slot = titleInHeader ? mobileHeaderTitleHost : desktopTitleSlotRef.current;
    if (slot !== null) {
      const focused = lastFocusedTitleRef.current;
      const restore = focused !== null
        && (document.activeElement === focused || document.activeElement === document.body);
      // Read controls change shape across breakpoints; the editor itself is
      // stable. Hand off only focus previously owned by this title subtree.
      const target = focused !== null && titleContainer.contains(focused) ? focused : titleReadControlRef.current;
      relocatingTitleRef.current = true;
      slot.appendChild(titleContainer);
      // A host removed at the breakpoint can drop focus to body without a
      // blur event. Restore the same input after moving its stable container.
      if (restore) target?.focus({ preventScroll: true });
      relocatingTitleRef.current = false;
    }
  }, [mobileHeaderTitleHost, titleContainer, titleInHeader]);

  const deletion = useDeleteConfirm((_id, signal) => onDeleteTrack(signal));
  const closedFeedback = useOperationFeedback();
  const dismissFeedback = useOperationFeedback();
  const [closedPending, setClosedPending] = useState(false);
  const notificationSignature = inputNotifications.map(({ key }) => key).join('|');
  const [noticeExpanded, setNoticeExpanded] = useState(inputNotifications.length > 0 && !conversationOpen);
  const [notificationAnnouncement, setNotificationAnnouncement] = useState('');
  const offeredClosedAction = canReopenTrack ? 'reopen' : (canCloseTrack ? 'close' : null);
  const closedPendingRef = useRef<'close' | 'reopen' | null>(null);
  const previousNotificationSignatureRef = useRef('');
  const previousNotificationCountRef = useRef(0);
  const previousConversationOpenRef = useRef(conversationOpen);
  useEffect(() => {
    // The PATCH promise settles before its invalidation refetch. Keep Close and
    // Reopen fenced after a successful response until the authoritative
    // capability that offered it disappears; otherwise the stale detail can
    // launch a second PATCH.
    if (closedPendingRef.current === null || closedPendingRef.current === offeredClosedAction) return;
    closedPendingRef.current = null;
    setClosedPending(false);
  }, [offeredClosedAction]);
  useEffect(() => {
    if (notificationSignature === previousNotificationSignatureRef.current) return;
    const count = inputNotifications.length;
    setNotificationAnnouncement(count > 0
      ? `${count} ${count === 1 ? 'notification needs' : 'notifications need'} your attention.`
      : (previousNotificationCountRef.current > 0 ? 'All notifications cleared.' : ''));
    if (count > 0 && !conversationOpen) setNoticeExpanded(true);
    previousNotificationSignatureRef.current = notificationSignature;
    previousNotificationCountRef.current = count;
  }, [conversationOpen, inputNotifications.length, notificationSignature]);
  useEffect(() => {
    if (conversationOpen && !previousConversationOpenRef.current) setNoticeExpanded(false);
    previousConversationOpenRef.current = conversationOpen;
  }, [conversationOpen]);
  const boardOpen = onCloseBoard !== undefined;
  const mobilePanelOpen = panel !== null;
  const noticePanelOpen = noticeExpanded;
  const mobilePanelKind: MobilePanelKind = panel ?? 'cards';
  const setTrackClosed = async (action: 'close' | 'reopen'): Promise<boolean> => {
    if (offeredClosedAction !== action || closedPendingRef.current !== null) return false;
    closedPendingRef.current = action;
    setClosedPending(true);
    const done = await closedFeedback.run(
      Promise.resolve().then(() => (action === 'close' ? onCloseTrack() : onReopenTrack())),
      `Could not ${action} this track.`,
    );
    if (!done) {
      closedPendingRef.current = null;
      setClosedPending(false);
    }
    return done;
  };
  const trackWorkActions = [
    ...(offeredClosedAction === 'reopen' ? [
      { label: 'Reopen', isDisabled: closedPending, onClick: () => setTrackClosed('reopen') },
    ] : []),
    ...(offeredClosedAction === 'close' ? [
      { label: 'Close', isDisabled: closedPending, onClick: () => setTrackClosed('close') },
    ] : []),
  ];
  const deleteTrackAction = { label: 'Delete track', onClick: () => deletion.request(track.id) };
  const trackMutationActions = [
    ...trackWorkActions,
    ...(offeredClosedAction !== null ? [{ type: 'divider' as const }] : []),
    deleteTrackAction,
  ];
  /* The desktop panel goes through `core/view`: one derivation, one traversal, one painter. This file may not spell a projection marker; `desktop-projection.test.tsx` scans for that. */
  /* `activity` is the track itself: `Track` carries the overlay-derived
     `cards` verdicts, and that field is all the derivation is typed to read. */
  const panelView = deriveTrackPageView({ cards, tasks, activity: track, openableCards });
  const desktopPainter = makeDesktopPainter({
    onOpenCard,
    onOpenTask,
    onDeleteCard,
    cardsAction,
    taskSummary: taskInventorySummary(tasks),
    afterCards: recentFiles,
  });
  const [desktopActionsOpen, setDesktopActionsOpen] = useState(false);
  const desktopActionsRef = useRef<HTMLButtonElement | null>(null);
  const mobilePanelRef = useRef<HTMLElement | null>(null);
  const mobileActionsRef = useRef<HTMLSpanElement | null>(null);
  const previousPanel = useRef<MobilePanelKind | null>(null);

  useEffect(() => {
    if (!mobilePanelOpen || mobilePanelObscured) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      // A dialog or workspace sheet owns Escape until its foreground layer closes.
      if (document.querySelector('[data-nc-escape-layer]') !== null) return;
      // Through the URL, not a local flag: Escape and the hardware Back button must end in the same place.
      onClosePanel?.();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [mobilePanelOpen, mobilePanelObscured, onClosePanel]);

  /* Focus: opening moves it into the panel container; closing returns it to the menu that opened it. Driven off `panel`, not the click, so the hardware Back button and a cold-start `?panel=` deep link behave too. */
  useEffect(() => {
    const previous = previousPanel.current;
    previousPanel.current = panel;
    if (panel !== null) {
      if (previous === null) mobilePanelRef.current?.focus({ preventScroll: true });
      return;
    }
    if (previous === null) return;
    mobileActionsRef.current?.querySelector('button')?.focus({ preventScroll: true });
  }, [panel]);

  /** Panel changes are immediate; the route remains their navigation owner. */
  const openMobilePanel = (kind: MobilePanelKind) => {
    onOpenPanel?.(kind);
  };
  const closeMobilePanel = () => {
    onClosePanel?.();
  };

  /* Rebuilt per render, like the desktop's: the page chrome it closes over —
     where Back goes — is a fact about this render. */
  const mobilePainter = makeMobilePainter({
    // Task reveal owns clearing its panel route; do not close it a second time.
    onOpenTask,
    backLabel: 'Report',
    onBack: closeMobilePanel,
  });

  const mobileActions = !boardOpen ? (
    <span className={styles.mobilePanelButton} ref={mobileActionsRef}>
      <AstryxMoreMenu
        label="Track actions"
        variant="ghost"
        size="lg"
        items={[
          ...(outlineItems.length > 0
            ? [{ label: 'Outline', onClick: () => openMobilePanel('outline') }]
            : []),
          /* The module sequence is this menu: derived from `rowModules`, so a module the view model gains, loses or reorders moves the menu with it. `Outline` and `Conversations` are not row modules. */
          ...panelView.rowModules.map((module) => ({
            label: module.title,
            onClick: () => openMobilePanel(module.key),
          })),
          { label: 'Conversations', onClick: () => openMobilePanel('conversations') },
          { type: 'divider' },
          ...trackWorkActions,
          ...(trackWorkActions.length > 0 ? [{ type: 'divider' as const }] : []),
          ...(canEditMobileTitle ? [{ label: 'Edit track', onClick: () => { requestAnimationFrame(() => mobileTitleEditRef.current?.()); } }] : []),
          deleteTrackAction,
        ]}
      />
    </span>
  ) : undefined;

  return (
    <section
      className={`${styles.page} ${boardOpen ? styles.pageBoard : ''}`}
      data-nc-track-page=""
    >
      <PageHeader
        title={
          <>
            {onCloseBoard !== undefined && (
              <button
                type="button"
                data-nc-role="icon"
                className={styles.headerBack}
                aria-label="Back to track"
                title="Back to track"
                onClick={onCloseBoard}
              >
                <Icon name="arrow-left" />
              </button>
            )}
            {/* The raw title, with the fallback as the placeholder. `emptyCommit="clear"`: the planner's `calm.track.rename` succeeds only while the title is empty, so clearing the name is a real request here, not a cancel. */}
            <div className={styles.titleSlot} ref={desktopTitleSlotRef} />
          </>
        }
        actions={(
          <span className={styles.headerActions}>
            <AstryxDropdownMenu
              button={{
                ref: desktopActionsRef,
                label: `Track actions for ${trackDisplayTitle(track.title)}`,
                icon: getAstryxIcon('moreHorizontal'),
                variant: 'ghost',
                size: 'sm',
                isIconOnly: true,
              }}
              items={trackMutationActions}
              hasChevron={false}
              isMenuOpen={desktopActionsOpen}
              onOpenChange={(isOpen) => {
                setDesktopActionsOpen(isOpen);
                if (!isOpen) {
                  requestAnimationFrame(() => desktopActionsRef.current?.focus());
                }
              }}
            />
          </span>
        )}
      />

      {createPortal(<div className={titleInHeader ? styles.mobileHeaderIdentity : styles.titleCluster}
        onFocusCapture={(event) => { lastFocusedTitleRef.current = event.target; }}
        onBlurCapture={(event) => {
          if (relocatingTitleRef.current) event.stopPropagation();
          else if (!titleContainer.contains(event.relatedTarget)) lastFocusedTitleRef.current = null;
        }}>
        <h1 className={styles.titleHeading}><EditableTitle value={track.title} placeholder={UNTITLED_TRACK_LABEL}
          displayContent={<TrackTitle track={track} />} emptyCommit="clear" onCommit={onRenameTrack} editLabel="Rename track" inputLabel="Track title"
          className={styles.title} isPageTitle titleRef={titleReadControlRef}
          readView={titleInHeader && mobileTitleReadView !== undefined ? (controls) => <MobileTitleReadView
            controls={controls} host={mobileTitleReadHost} view={mobileTitleReadView} register={registerMobileTitleEdit} /> : undefined} /></h1>
        <div className={styles.mobileTitleReadHost} hidden={!titleInHeader}
          ref={(node) => { if (node !== null && mobileTitleReadHost.parentNode !== node) node.appendChild(mobileTitleReadHost); }} />
        {!titleInHeader && <TrackClosedBadge closedAt={track.closedAt} />}
        {/* The same indicator the rail paints. `unread` is `false` by construction: the page is the reader, and its receipt clears the moment it is visible. The mobile page head carries no indicator. */}
        {!titleInHeader && <ActivityIndicator state={trackActivityState(track, false)} />}
      </div>, titleContainer)}
      {headerActionsHost !== null && mobileActions !== undefined && createPortal(mobileActions, headerActionsHost)}
      {titleInHeader && !boardOpen ? null : <div className={styles.mobileTrackHeader}>
        <MobileHeader
          title={trackDisplayTitle(track.title)}
          titleText={<TrackTitle track={track} />}
          meta={<TrackClosedBadge closedAt={track.closedAt} />}
          level={1}
          backLabel={boardOpen ? 'Report' : mobileBackLabel}
          onBack={boardOpen ? onCloseBoard : onMobileBack}
          actions={headerActionsHost === null ? mobileActions : undefined}
        />
      </div>}

      <div className={styles.workspace}>
      <div
        className={styles.content}
        aria-hidden={boardOpen ? true : undefined}
        inert={boardOpen}
      >
        <div className={styles.doc}>{report}</div>

        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open. */}
        <aside
          id="mobile-track-panel"
          ref={mobilePanelRef}
          className={`${styles.panel} ${mobilePanelOpen ? styles.mobilePanelOpen : styles.mobilePanelClosed}`}
          data-nc-panel=""
          data-nc-mobile-page={mobilePanelOpen ? 'open' : 'closed'}
          /* Programmatically focusable only — the container is where focus
             lands when the panel opens, never a Tab stop of its own. */
          tabIndex={mobilePanelOpen ? -1 : undefined}
        >
          <div
            className={styles.mobileListSurface}
            /* The projection's root on this surface: the two surfaces are siblings in the DOM at the same time, so a whole-page scan would read them as one tree. */
            data-nc-mobile-panel=""
            aria-hidden={mobilePanelOpen && !(compactViewport && mobilePanelObscured) ? undefined : true}
            inert={!mobilePanelOpen || (compactViewport && mobilePanelObscured)}
          >
            {mobilePanelOpen && (mobilePanelKind === 'outline' ? (
              <MobileListPage
                title="Outline"
                backLabel="Report"
                onBack={closeMobilePanel}
              >
                <MobileList>
                  {outlineItems.flatMap((item) => [
                    <MobileListItem
                      key={item.blockId}
                      title={item.label}
                      meta={item.number === null ? undefined : String(item.number)}
                      onSelect={() => {
                        // The anchor navigation clears `?panel=` itself, so closing it here too would be two moves for one.
                        onOpenOutline?.(item.blockId);
                      }}
                    />,
                    ...item.children.map((child) => (
                      <MobileListItem
                        key={child.blockId}
                        title={child.label}
                        nested
                        onSelect={() => {
                          onOpenOutline?.(child.blockId);
                        }}
                      />
                    )),
                  ])}
                </MobileList>
              </MobileListPage>
            ) : mobilePanelKind === 'conversations' ? (
              <MobileListPage
                title="Conversations"
                backLabel="Report"
                onBack={closeMobilePanel}
              >
                <div className={styles.mobileConversationList}>
                  {conversationList ?? <p>No conversations yet.</p>}
                </div>
              </MobileListPage>
            ) : (
              /* Every row module goes through `core/view`, in one branch: what is left of `MobilePanelKind` is exactly `RowModuleView['key']`. The four declaration words are deliberately not quoted anywhere in this file. */
              paintMobileModule(mobilePainter, rowModule(panelView, mobilePanelKind))
            ))}
          </div>
          <div
            className={styles.desktopPanelSurface}
            /* The projection's root on this surface; `.mobileListSurface` is a sibling in the DOM at the same time. */
            data-nc-desktop-panel=""
            aria-hidden={mobilePanelOpen ? true : undefined}
            inert={mobilePanelOpen}
          >
          <PanelCard>
            {paintDesktopPanel(desktopPainter, panelView)}
            {/* `REFERENCED BY` is absent, not empty, when nothing cites this track. */}
            {backlinks !== undefined && (
              <PanelModule title="Referenced by">{backlinks}</PanelModule>
            )}
            <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
          </PanelCard>
          </div>
        </aside>
      </div>
      {board}
      </div>

      {!boardOpen && !mobilePanelOpen && onStartConversation !== undefined && (
        <AstryxButton
          className={styles.mobileReportChatFab}
          data-nc-mobile-report-chat=""
          label="Chat"
          variant="primary"
          size="lg"
          icon={<Icon name="chat" />}
          onClick={onStartConversation}
        />
      )}

      <VisuallyHidden
        role="status"
        aria-label="Input notifications"
        aria-live="polite"
        aria-atomic="true"
      >{notificationAnnouncement}</VisuallyHidden>

      {inputNotifications.length > 0 && (
        <aside
          className={`${styles.needsInputNotice} ${noticePanelOpen
            ? styles.needsInputNoticeExpanded
            : styles.needsInputNoticeCompact} ${conversationOpen ? styles.needsInputNoticeBesideDrawer : ''}`}
          data-nc-needs-input-notice=""
          data-nc-notification-mode={noticePanelOpen ? 'expanded' : 'compact'}
          role="region"
          aria-label="Notifications"
        >
          {noticePanelOpen ? (
            <>
              <span className={styles.needsInputNoticeHeader}>
                <span className={styles.needsInputNoticeIcon}><Icon name="notification" size="sm" /></span>
                <strong className={styles.needsInputNoticeTitle}>Waiting on you</strong>
                <span className={styles.needsInputCount}>{inputNotifications.length}</span>
                <button
                  type="button"
                  className={styles.needsInputCollapse}
                  aria-label="Collapse notifications"
                  title="Collapse notifications"
                  onClick={() => setNoticeExpanded(false)}
                ><Icon name="chevron-right" size="sm" /></button>
              </span>
              <ul className={styles.needsInputNoticeList}>
                {inputNotifications.map((notification) => (
                  <li
                    key={notification.key}
                    className={`${styles.notice} ${onReply === undefined ? '' : styles.noticeActionable}`}
                    data-nc-notification-state={notification.kind}
                  >
                    {/* The whole row is the reply: a button stretched over it, so nothing interactive nests
                        inside another; the × and the body's links sit above it. */}
                    {onReply !== undefined && (
                      <button
                        type="button"
                        className={styles.noticeOpen}
                        aria-label={`${NOTIFICATION_ACTION[notification.kind].name}: ${notificationGist(notification.text)}`}
                        onClick={onReply}
                      />
                    )}
                    <span className={styles.noticeMeta}>
                      <span
                        className={`${styles.noticeDot} ${notification.kind === 'ask' ? styles.noticeDotAsk : styles.noticeDotDown}`}
                        aria-hidden="true"
                      />
                      {/* One slot, two spans: the label, and the row's action in its place while the row is hovered
                          or focused. Stacked in one grid cell, so the swap does not move the time. */}
                      <span className={styles.noticeLabel}>
                        <span className={styles.noticeLabelText}>{NOTIFICATION_LABEL[notification.kind]}</span>
                        {onReply !== undefined && (
                          <span className={styles.noticeAction} aria-hidden="true">{NOTIFICATION_ACTION[notification.kind].hint}</span>
                        )}
                      </span>
                      <time className={styles.noticeTime} dateTime={new Date(notification.atMs).toISOString()}>
                        {relativeTime(notification.atMs, nowMs ?? Date.now())}
                      </time>
                    </span>
                    {/* The kernel's words are markdown, rendered as the chat renders a reply. An ask is read whole;
                        a failure is clamped, its full text is in the Planner conversation. */}
                    <div className={`${styles.noticeBody} ${notification.kind === 'planner-down' ? styles.noticeBodyClamped : ''}`}>
                      <Markdown density="compact" headingLevelStart={3}>{notification.text}</Markdown>
                    </div>
                    {onDismiss !== undefined && (
                      <button
                        type="button"
                        className={styles.noticeDismiss}
                        aria-label={`Dismiss: ${NOTIFICATION_LABEL[notification.kind]}: ${notificationGist(notification.text)}`}
                        title="Dismiss"
                        onClick={() => {
                          void dismissFeedback.run(onDismiss(notification.key), 'Could not dismiss this notification.');
                        }}
                      ><Icon name="close" size="sm" /></button>
                    )}
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <button
              type="button"
              className={styles.needsInputLauncher}
              data-nc-notification-launcher=""
              aria-label={`Open ${inputNotifications.length} ${inputNotifications.length === 1 ? 'notification' : 'notifications'}`}
              title="Open notifications"
              onClick={() => setNoticeExpanded(true)}
            >
              <Icon name="notification" />
              <span className={styles.needsInputIndicator} aria-hidden="true">{inputNotifications.length}</span>
            </button>
          )}
        </aside>
      )}

      <ConfirmDialog
        open={deletion.open}
        title={DELETE_TRACK_COPY.title}
        description={DELETE_TRACK_COPY.description}
        confirmLabel={DELETE_TRACK_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={deletion.pending ? 'busy' : 'ready'}
        onConfirm={deletion.confirm}
        onCancel={deletion.cancel}
      />
      <OperationFeedback feedback={deletion.feedback} />
      <OperationFeedback feedback={closedFeedback} />
      <OperationFeedback feedback={dismissFeedback} />
    </section>
  );
}
