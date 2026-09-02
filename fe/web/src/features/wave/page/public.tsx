// The `/wave/$waveId` surface.
//
// §8.3 — the one page where the product is actually alive, and the one the
// rewrite is most missing. The card runtime is a later slice; that is a fact to
// be **rendered honestly** at the real geometry, not narrated in prose. The
// document slot therefore occupies a document's worth of space even when empty,
// because that shape is what teaches the user what they will get.
//
// Presentational: the wave, its cove and its cards arrive as props, and every
// mutation and every navigation leaves through a callback — `features/**` may
// not import `app/**`, so the router owns both the fetch and the destination.
//
// INV-A11Y-061 — every navigation here is a `<button>` plus a callback. There
// is no `<a href>` anywhere on this page, and `public.contract.test.tsx` holds
// that line for the whole subtree.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { MoreMenu as AstryxMoreMenu } from '@astryxdesign/core/MoreMenu';
import { useEffect, useRef, type ReactNode } from 'react';

import type { ReportOutlineItem, ReportTaskRow } from '../../../../../core/domain/report.ts';
import {
  UNTITLED_WAVE_LABEL, waveDisplayTitle, type CardWire, type Wave, type WaveLifecycle,
} from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { MobileList, MobileListItem, MobileListPage } from '../../../ui/mobile-list/public.tsx';
import { MobileHeader } from '../../../ui/mobile-header/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { OperationFeedback, useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { PanelCard, PanelModule } from '../../../ui/panel-card/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { deriveWavePageView } from '../../../../../core/view/wave-page.ts';
import type { RowModuleView, WavePageView } from '../../../../../core/view/panel.ts';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import { makeDesktopPainter, paintDesktopPanel } from './desktop-painter.tsx';
import { makeMobilePainter, paintMobileModule } from './mobile-painter.tsx';
import styles from './page.module.css';

/**
 * The four mobile drill-down pages.
 *
 * **Two of them are named by the view model, and are spelled that way** (Δ2):
 * `RowModuleView['key']` rather than `'cards' | 'tasks'`, so a row module the
 * derivation gains or renames is a type error here instead of a menu entry that
 * quietly opens nothing. The other two are not row modules — `Outline` reads the
 * report's anchors and `Conversations` is a router-composed slot — and they are
 * written out, because pushing them into `rowModules` to make one loop out of
 * four entries would let this page's navigation decide the view model's
 * contents.
 */
type MobilePanelKind = 'outline' | RowModuleView['key'] | 'conversations';

export type WavePageProps = Readonly<{
  wave: Wave;
  cards: readonly CardWire[];
  /**
   * The wave's tasks, joined by `app/router` from the report's own `task`
   * blocks and the kernel's task verdicts (`core/domain/report`'s
   * `deriveReportTasks`) — the declarations stay the spine, so this is still
   * not a second source of truth about what tasks exist, and each row now
   * carries what the kernel says about the run.
   */
  tasks: readonly ReportTaskRow[];
  /** Report anchors rendered as a separate mobile list instead of a margin rail. */
  outlineItems?: readonly ReportOutlineItem[];
  /** The panel card's second module, composed by `app/router` (features/chat). */
  /** The report document, composed by `app/router` (features/report). */
  report?: ReactNode;
  /** `REFERENCED BY` — omitted entirely when nothing cites this wave (§6.1). */
  backlinks?: ReactNode;
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Starts Chat from the mobile Report's dedicated floating action. */
  onStartConversation?: () => void;
  /** The Cards module head's `+`, composed by `app/router`. */
  cardsAction?: ReactNode;
  onOpenCard?: (cardId: string) => void;
  /**
   * Supplying this reveals a delete on every row the kernel says is deletable —
   * the same shape `features/wave/row` uses for a wave. The caller owns the
   * confirm, because the board offers the identical gesture on the card's own
   * head and the two must not each grow a dialog of their own.
   */
  onDeleteCard?: (cardId: string) => void;
  /** Reveal a task's block in the document — the same landing the outline, a
   *  `neige://` link and a backlink all use. */
  onOpenTask?: (blockId: string) => void;
  onOpenOutline?: (blockId: string) => void;
  /**
   * The card grid, composed by `app/router`. Positioned over the document
   * column so the page title stays the only title bar — a second overlay
   * header was overflowing the measure.
   */
  board?: ReactNode;
  onCloseBoard?: () => void;
  /**
   * Which secondary panel the mobile report is showing, or `null` for none.
   *
   * The panel is a navigation *destination*, so its identity lives in the URL
   * (`?panel=`, #1191 §1) and `app/router` reads it — `features/**` may not
   * import `app/**`, and this page stays a pure renderer.
   *
   * There used to be a fifth mobile page below — a card **detail** held in
   * component state rather than the URL, because its legal set included cards
   * `?card=` would bounce off. #1234 S1b-4a deleted it: opening a card is not
   * offered on this viewport (`mobile-painter.tsx`'s capability table), so the
   * page it drilled into had nothing left to be.
   */
  panel?: MobilePanelKind | null;
  onOpenPanel?: (panel: MobilePanelKind) => void;
  onClosePanel?: () => void;
  mobileBackLabel?: string;
  onMobileBack?: () => void;
  /** CR-8 — after a successful delete, focus lands on the cove page's title. */
  onRenameWave: (title: string) => void | Promise<void>;
  onDeleteWave: (signal: AbortSignal) => void | Promise<void>;
}>;


/** Draft / done / canceled are idle facts; they do not earn a header badge. */
function headerLifecycle(lifecycle: WaveLifecycle): WaveLifecycle | null {
  if (lifecycle === 'draft' || lifecycle === 'done' || lifecycle === 'canceled') return null;
  return lifecycle;
}

/**
 * The view model's module under `key`.
 *
 * A lookup by key rather than by index: `deriveWavePageView` derives both row
 * modules, and reading one out by position would bind this page to the
 * derivation's *order* — a fact it has no business knowing, and the same
 * re-derivation `desktop-painter.tsx` avoided by carrying `parts.key` through
 * its leaves. Missing is an error rather than an empty page, because a mobile
 * page silently rendering nothing is precisely how a surface goes missing
 * without anything noticing.
 */
function rowModule(view: WavePageView, key: RowModuleView['key']): RowModuleView {
  const found = view.rowModules.find((module) => module.key === key);
  if (found === undefined) throw new Error(`the wave page view has no ${key} module`);
  return found;
}

export function WavePage({
  wave, cards, tasks, outlineItems = [], report, backlinks, conversationList, conversationAction,
  onStartConversation,
  cardsAction, onOpenCard, onDeleteCard, onOpenTask, onOpenOutline, board, onCloseBoard,
  panel = null, onOpenPanel, onClosePanel,
  mobileBackLabel = 'Pages', onMobileBack,
  onRenameWave, onDeleteWave,
}: WavePageProps) {
  const deletion = useDeleteConfirm((_id, signal) => onDeleteWave(signal));
  const boardOpen = onCloseBoard !== undefined;
  const mobilePanelOpen = panel !== null;
  const mobilePanelKind: MobilePanelKind = panel ?? 'cards';
  /** The drill-down page's entrance animation — a *panel* fact, not a card one:
   *  all four mobile pages take it, and `openMobilePanel` sets it. (The card
   *  detail page it was named after is gone; the motion is not.) */
  const [mobileCardMotion, setMobileCardMotion] = useState<'none' | 'forward' | 'back'>('none');
  const lifecycle = headerLifecycle(wave.lifecycle);
  /*
   * ── The desktop panel goes through `core/view` (#1234 S1b-3b) ────────────
   *
   * One derivation, one traversal, one painter. What this file used to do — walk
   * `cards` and `tasks` itself and spell each row's DOM inline — is what let the
   * two viewports drift apart in the first place.
   *
   * **This file may not spell a projection marker.** Not one of the six
   * attribute names in `core/view/panel.ts`'s `MARKER` table, in either their
   * attribute spelling or their `dataset` one.
   * `desktop-projection.test.tsx` asserts that absence mechanically, over this
   * file's own source and in both spellings — which is why this comment names
   * none of them.
   *
   * **What that scan is, exactly.** It stops this file from *rewriting a marker
   * literal in place*, which is the cheap way the panel would drift back into
   * being hand-composed. It is **not** a proof that the painter ran: a marker
   * can reach the DOM from here with no literal at all — a computed property, a
   * concatenation, a marker-channel prop (`ui/panel-card` takes three), or a
   * component imported from a file that carries markers of its own. The claim
   * that this page goes *through* `paintDesktopPanel` and renders what it hands
   * back is `desktop-entry.test.tsx`'s, and it is held by holding the call
   * rather than by any marker's spelling.
   *
   * The page's other markers (`data-nc-wave-page`, `data-nc-role`,
   * `data-nc-panel`, the two inventory markers, …) are this page's own and stay.
   *
   * **Since S1b-4a/4b both mobile row-module pages are renderers of the same
   * derivation** (`paintMobileModule`, one call per drill-down), with
   * `mobile-entry.test.tsx` holding those calls the way `desktop-entry.test.tsx`
   * holds the desktop's. `Outline` and `Conversations` are not row modules and
   * stay hand-composed: they are not in `rowModules`, and pushing them in would
   * let this page's navigation decide the view model's contents.
   */
  const panelView = deriveWavePageView({ cards, tasks });
  const desktopPainter = makeDesktopPainter({ onOpenCard, onOpenTask, onDeleteCard, cardsAction });
  const mobilePanelRef = useRef<HTMLElement | null>(null);
  const mobileActionsRef = useRef<HTMLSpanElement | null>(null);
  const previousPanel = useRef<MobilePanelKind | null>(null);

  /*
   * Opening the card grid leaves no drill-down animation queued behind it. The
   * *panel* needs no closing here: `?card=` and `?panel=` are mutually
   * exclusive by construction, and `app/router` gives the card precedence when
   * both somehow appear (§0.1).
   */
  useEffect(() => {
    if (!boardOpen) return;
    setMobileCardMotion('none');
  }, [boardOpen]);

  useEffect(() => {
    if (!mobilePanelOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      // Through the URL, not a local flag: Escape and the hardware Back
      // button must end in the same place (#1191 §2.4).
      setMobileCardMotion('none');
      onClosePanel?.();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [mobilePanelOpen, onClosePanel]);

  /*
   * ── The focus contract (#1191 §2.5) ─────────────────────────────────────
   *
   * Opening moves focus into the panel container; closing returns it to the
   * three-dot menu that opened it. Both are driven off `panel` rather than off
   * the click, which is what makes the *hardware Back button* (a POP that only
   * changes the URL) restore focus too, and what makes a cold-start `?panel=`
   * deep link land the reader inside the panel on first paint.
   *
   * Swapping one panel for another is not a transition: focus is already in
   * the container the swap redraws.
   */
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

  /** Every entry into a panel: the page slides in from the trailing edge. */
  const openMobilePanel = (kind: MobilePanelKind) => {
    setMobileCardMotion('forward');
    onOpenPanel?.(kind);
  };
  /** Leaving the panel by a navigation that clears `?panel=` on its own (§1.4). */
  const leaveMobilePanel = () => {
    setMobileCardMotion('none');
  };
  const closeMobilePanel = () => {
    leaveMobilePanel();
    onClosePanel?.();
  };

  /* Rebuilt per render, like the desktop's: the page chrome it closes over —
     where Back goes, how the page animates in — is a fact about this render. */
  const mobilePainter = makeMobilePainter({
    /* The reveal navigation clears `?panel=` itself (§1.4), so the panel is left
       rather than closed — closing it here too would be two moves for one. That
       wrapper is why the painter takes a handler instead of the page's prop. */
    onOpenTask: (blockId) => {
      leaveMobilePanel();
      onOpenTask?.(blockId);
    },
    backLabel: 'Report',
    onBack: closeMobilePanel,
    motion: mobileCardMotion,
  });

  const mobileActions = !boardOpen ? (
    <span className={styles.mobilePanelButton} ref={mobileActionsRef}>
      <AstryxMoreMenu
        label="Wave actions"
        variant="ghost"
        size="lg"
        items={[
          ...(outlineItems.length > 0
            ? [{ label: 'Outline', onClick: () => openMobilePanel('outline') }]
            : []),
          /*
            ── Δ2: the module sequence is this menu ─────────────────────────
            *
            * On the desktop the two row modules are a DOM sequence and
            * `paintPanel` walks it. Mobile drills into one at a time, so the
            * sequence has no traversal to live in — it is a *navigation*
            * structure, and until this slice it was two written-out entries
            * that happened to agree with `deriveWavePageView`. Agreeing by
            * coincidence is what the whole issue is about: the statement
            * "**both surfaces show the same row modules, in the same order**"
            * had no carrier on this side at all.
            *
            * It has one now — the derivation itself. A module the view model
            * gains appears here; one it loses disappears; a reorder reorders
            * the menu. `public.test.tsx`'s "the drill-down menu offers exactly
            * the derived row modules" is what makes that mechanical, by
            * comparing the menu against `rowModules` and following each entry
            * into the page it opens.
            *
            * `Outline` above and `Conversations` below stay hand-written: they
            * are not row modules (see `MobilePanelKind`).
          */
          ...panelView.rowModules.map((module) => ({
            label: module.title,
            onClick: () => openMobilePanel(module.key),
          })),
          { label: 'Conversations', onClick: () => openMobilePanel('conversations') },
          { type: 'divider' },
          { label: 'Delete wave', onClick: () => deletion.request(wave.id) },
        ]}
      />
    </span>
  ) : undefined;

  return (
    <section
      className={`${styles.page} ${boardOpen ? styles.pageBoard : ''}`}
      data-nc-wave-page=""
    >
      {/*
        One row, like every other route.

        The breadcrumb row is gone. "Today / ● atlas" cost a full row of chrome
        on every visit to restate two things the rail states permanently and in
        the same words — the rail is a tree with the cove above the wave, and
        the current wave is the row marked in it. A breadcrumb earns its place
        where the ancestor is otherwise unreachable; here it never was.

        The cove dot went with it. It was the only colour in the header, and it
        was sitting next to the cove's own name — the same defect the cove page
        header had, one level down.
      */}
      {/*
        Page-aligned, not document-aligned. `align="document"` pushed the
        title into the centred prose column, so it sat in the middle while
        the outline hung in the empty gutter to its left and the panel card
        sat on the right. The title belongs on the page's leading edge; the
        outline then lives under this header in that same gutter.
      */}
      <PageHeader
        title={
          <>
            {onCloseBoard !== undefined && (
              <button
                type="button"
                data-nc-role="icon"
                className={styles.headerBack}
                aria-label="Back to wave"
                title="Back to wave"
                onClick={onCloseBoard}
              >
                <Icon name="arrow-left" />
              </button>
            )}
            {/*
              The **raw** title, with the fallback handed over as the
              placeholder (#1211). `waveDisplayTitle(wave.title)` here used to
              be both, so the editor opened on an unnamed wave holding the
              words `Untitled wave` — text the reader had to delete before
              typing. The header still reads the same; only the box changed.

              `emptyCommit="clear"` is the other half: a wave has a second
              namer (the spec agent's `calm.wave.rename`, which succeeds only
              while the title is empty), so clearing the name is a real request
              here and not the cancel it is on a cove.
            */}
            <h1 className={styles.titleHeading}><EditableTitle
              value={wave.title}
              placeholder={UNTITLED_WAVE_LABEL}
              emptyCommit="clear"
              onCommit={onRenameWave}
              editLabel="Rename wave"
              inputLabel="Wave title"
              className={styles.title}
              isPageTitle
            /></h1>
          </>
        }
        meta={
          <>
            {lifecycle !== null && <WaveLifecycleBadge lifecycle={lifecycle} />}
            {wave.anyCardNeedsInput && (
              <span className={styles.needsInput}>
                <span className={styles.needsInputDot} aria-hidden="true" />
                Needs input
              </span>
            )}
          </>
        }
        actions={(
          <button
            type="button"
            data-nc-role="icon"
            className={styles.headerDelete}
            aria-label={`Delete wave ${waveDisplayTitle(wave.title)}`}
            title="Delete wave"
            onClick={() => deletion.request(wave.id)}
          >
            <Icon name="close" />
          </button>
        )}
        /*
         * No identity row — `--header-h` is 62 here now, not 92.
         *
         * The folder is real (unlike the cove page's, which was synthesised
         * from whatever its waves happened to agree on, and was deleted). But
         * being real is not the same as belonging in the chrome: a page header
         * is what you read on *every* visit, and a path you already chose when
         * you created the wave is something you look up once, if ever. Three
         * rows of header to carry it was the page paying its largest fixed
         * cost for its least-read fact — and in mono, which §2.2 reserves for
         * machine identity precisely so it stands out where it matters.
         *
         * It moves to the panel, under a label, next to the other things about
         * this wave you might want to check. Nothing is lost; it stops being
         * chrome.
         */
      />

      <div className={styles.mobileWaveHeader}>
        <MobileHeader
          title={waveDisplayTitle(wave.title)}
          level={1}
          backLabel={mobileBackLabel}
          onBack={onMobileBack}
          actions={mobileActions}
        />
      </div>

      <div className={styles.workspace}>
      <div className={styles.content}>
        {/*
          The wave report, composed by `app/router` from this wave's
          `wave-report` card. No frame around it: a document is the column, and
          drawing a box told you where a thing you cannot yet have would end.
          It owns its own empty state.
        */}
        <div className={styles.doc}>{report}</div>

        {/*
          The same card every route has. CARDS is this route's own module —
          `kind` is a card's identity and `title` its label, and the row now
          prints BOTH: the title reads first, the kind follows in the quiet
          rank. It used to print `title ?? kind`, which was harmless while the
          only titled cards were ones a user had named — but #1149 titles every
          worker card after its task key, so `title ?? kind` would have deleted
          the words `codex` / `claude` / `terminal` from the panel on exactly
          the cards whose kind the reader most needs (three workers named after
          three slices are otherwise indistinguishable). An untitled card still
          shows its kind alone, in the same slot, because there is nothing else
          to say about it. FOLDER sits with it because a wave's cwd is a fact
          about the same object; it left the page header, where it was the
          largest fixed cost the page paid for its least-read fact.
        */}
        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open — see the same marker on the cove page. */}
        <aside
          id="mobile-wave-panel"
          ref={mobilePanelRef}
          className={`${styles.panel} ${mobilePanelOpen ? styles.mobilePanelOpen : styles.mobilePanelClosed}`}
          data-nc-panel=""
          data-nc-mobile-page={mobilePanelOpen ? 'open' : 'closed'}
          /* Programmatically focusable only — the container is where focus
             lands when the panel opens (§2.5), never a Tab stop of its own. */
          tabIndex={mobilePanelOpen ? -1 : undefined}
        >
          <div
            className={styles.mobileListSurface}
            /* The projection's root on this surface, and the mirror of
               `data-nc-desktop-panel` below: the two surfaces are siblings and
               are in the DOM at the same time, so a whole-page scan would read
               them as one tree now that both carry markers. */
            data-nc-mobile-panel=""
            aria-hidden={mobilePanelOpen ? undefined : true}
            inert={!mobilePanelOpen}
          >
            {mobilePanelOpen && (mobilePanelKind === 'outline' ? (
              <MobileListPage
                title="Outline"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={closeMobilePanel}
              >
                <MobileList>
                  {outlineItems.flatMap((item) => [
                    <MobileListItem
                      key={item.blockId}
                      title={item.label}
                      meta={item.number === null ? undefined : String(item.number)}
                      onSelect={() => {
                        // The anchor navigation clears `?panel=` itself (§1.4),
                        // so closing it here too would be two moves for one.
                        leaveMobilePanel();
                        onOpenOutline?.(item.blockId);
                      }}
                    />,
                    ...item.children.map((child) => (
                      <MobileListItem
                        key={child.blockId}
                        title={child.label}
                        nested
                        onSelect={() => {
                          leaveMobilePanel();
                          onOpenOutline?.(child.blockId);
                        }}
                      />
                    )),
                  ])}
                </MobileList>
              </MobileListPage>
            ) : mobilePanelKind === 'cards' ? (
              /*
                ── The mobile Cards page goes through `core/view` (#1234 S1b-4a) ──
                *
                * One derivation, one painter, one module. What this branch used
                * to do — walk `cards` itself and spell each row inline — is the
                * half of the drift the desktop's own slice could not reach: an
                * untitled card printed its kind twice here and nowhere else,
                * `kernel-owned` was missing, and the row opened a detail page
                * the desktop has no counterpart for.
                *
                * **One module, not the panel**: mobile drills into a module at
                * a time, so this is `paintModule` (Δ2), and the module sequence
                * lives in the navigation menu above.
                *
                * The two card actions are gone by decision, not by
                * omission — see `mobile-painter.tsx`'s capability table.
              */
              paintMobileModule(mobilePainter, rowModule(panelView, 'cards'))
            ) : mobilePanelKind === 'tasks' ? (
              /*
                ── The mobile Tasks page goes through `core/view` (#1234 S1b-4b) ──
                *
                * The other half of the drift, and the louder one: this branch
                * used to re-word `task.state` into four declaration words of its
                * own, so a ready task carried one here and none on the desktop,
                * and a dispatched one carried its readiness word here while the
                * desktop showed the run. Both rules are `deriveReportTasks`' and
                * now arrive through the derivation — a visible change, on the
                * record as D8.
                *
                * The four words are deliberately not quoted anywhere in this
                * file: `mobile-projection.test.tsx`'s wording hygiene guard
                * scans this source for them.
              */
              paintMobileModule(mobilePainter, rowModule(panelView, 'tasks'))
            ) : (
              <MobileListPage
                title="Conversations"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={closeMobilePanel}
              >
                <div className={styles.mobileConversationList}>
                  {conversationList ?? <p>No conversations yet.</p>}
                </div>
              </MobileListPage>
            ))}
          </div>
          <div
            className={styles.desktopPanelSurface}
            /* The projection's root on this surface. `.mobileListSurface` is a
               sibling and is in the DOM at the same time (the desktop side only
               takes `inert`), and since S1b-4a/4b it carries markers of its own,
               so a whole-page scan would now mix the two. Each surface's scan is
               rooted at its own marker: this attribute, and
               `data-nc-mobile-panel` opposite. */
            data-nc-desktop-panel=""
            aria-hidden={mobilePanelOpen ? true : undefined}
            inert={mobilePanelOpen}
          >
          <PanelCard>
            {/*
              ── FOLDER became TASKS ─────────────────────────────────────────
              *
              * FOLDER said "you choose it once at creation and the app never
              * showed it again". That stopped being true in the other
              * direction: **nobody chooses it any more.** `cove/new-wave` omits
              * `cwd` and `attach_folder` from the create POST, so the kernel
              * takes the #1131 branch — persist `default_cwd()` (`$HOME`) and
              * skip the `cove_folders` claim entirely. Every wave this
              * front-end creates therefore reports the same `$HOME`, and a
              * module whose value is a constant the reader neither picked nor
              * can change is a row that costs a module and answers nothing.
              *
              * `cwd` itself is not dead and is not removed — workers and gates
              * run there, and the task blocks below carry it. What is removed
              * is showing it as though it were a decision.
              *
              * What takes the slot is the thing that *is* a fact about this
              * wave and had nowhere to be read: its tasks. They are report
              * blocks (#229), scattered through the prose at the point the
              * agent declared them, each carrying a worker prompt and gate
              * commands. As an inventory they belong in the panel; as
              * declarations they belong in the document. A row here is a
              * pointer to the block, not a copy of it — clicking reveals the
              * block, which is the same landing every outline row, `neige://`
              * link and backlink already uses.
              *
              * Still not a file browser, and §8.3's reasoning for cutting FILES
              * is untouched: evidence a report wants you to see belongs in the
              * report, as a block.
              *
              * Both row modules — Cards and Tasks, in the view model's order —
              * are one traversal now. Their DOM lives in `desktop-painter.tsx`,
              * which is where the reasoning about each row's shape moved with
              * it.
            */}
            {paintDesktopPanel(desktopPainter, panelView)}
            {/*
              `REFERENCED BY` is absent, not empty, when nothing cites this wave
              (§6.1: a section with zero rows is not rendered). Being uncited is
              the normal state of a new wave, and a permanent "No backlinks yet."
              would make the common case look like a deficiency.
            */}
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

      <ConfirmDialog
        open={deletion.open}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={deletion.pending ? 'busy' : 'ready'}
        onConfirm={deletion.confirm}
        onCancel={deletion.cancel}
      />
      <OperationFeedback feedback={deletion.feedback} />
    </section>
  );
}
