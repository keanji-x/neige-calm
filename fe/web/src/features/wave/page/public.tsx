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
import { useEffect, type ReactNode } from 'react';

import type { ReportOutlineItem, ReportTaskRow } from '../../../../../core/domain/report.ts';
import { waveDisplayTitle, type CardWire, type Wave, type WaveLifecycle } from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../../ui/mobile-list/public.tsx';
import { MobileHeader } from '../../../ui/mobile-header/public.tsx';
import { setMobileSecondaryOpen, subscribeMobilePageRoot } from '../../../ui/mobile-page/public.ts';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { OperationFeedback, useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import styles from './page.module.css';

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
 * What the status dot says, in words: the status, then the kernel's reason for
 * it when there is one (#1149 / #1147).
 *
 * The status word comes **first and always**, because this string is the dot's
 * whole accessible name — the colour carries nothing on its own, and a reader
 * who lands here must get `failed` before any prose about it. The reason is
 * appended, never substituted: `failed — wave … is not a git repository` is
 * strictly more than `failed`, whereas a name that printed the reason alone
 * would have traded the one fact the row must carry for a nicer one.
 *
 * The em dash separator is the only formatting decision here; the reason
 * arrives already collapsed to one bounded line from `deriveReportTasks`, which
 * is where that judgement belongs.
 */
function taskStatusPhrase(status: string, detail: string | null): string {
  return detail === null ? status : `${status} — ${detail}`;
}

type MobilePanelKind = 'outline' | 'cards' | 'tasks' | 'conversations';

export function WavePage({
  wave, cards, tasks, outlineItems = [], report, backlinks, conversationList, conversationAction,
  onStartConversation,
  cardsAction, onOpenCard, onOpenTask, onOpenOutline, board, onCloseBoard,
  mobileBackLabel = 'Pages', onMobileBack,
  onRenameWave, onDeleteWave,
}: WavePageProps) {
  const deletion = useDeleteConfirm((_id, signal) => onDeleteWave(signal));
  const boardOpen = onCloseBoard !== undefined;
  const [mobilePanelOpen, setMobilePanelOpen] = useState(false);
  const [mobilePanelKind, setMobilePanelKind] = useState<MobilePanelKind>('cards');
  const [mobileCardId, setMobileCardId] = useState<string | null>(null);
  const [mobileCardMotion, setMobileCardMotion] = useState<'none' | 'forward' | 'back'>('none');
  const mobileCard = mobileCardId === null ? undefined : cards.find((card) => card.id === mobileCardId);
  const lifecycle = headerLifecycle(wave.lifecycle);

  useEffect(() => {
    if (boardOpen && mobilePanelOpen) {
      setMobilePanelOpen(false);
      setMobileCardId(null);
      setMobileCardMotion('none');
    }
  }, [boardOpen, mobilePanelOpen]);

  useEffect(() => {
    if (!mobilePanelOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      if (mobileCardId !== null) {
        setMobileCardMotion('back');
        setMobileCardId(null);
      }
      else setMobilePanelOpen(false);
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [mobileCardId, mobilePanelOpen]);

  useEffect(() => subscribeMobilePageRoot(() => {
    setMobileCardId(null);
    setMobileCardMotion('none');
    setMobilePanelOpen(false);
  }), []);

  useEffect(() => {
    setMobileSecondaryOpen(true);
    return () => setMobileSecondaryOpen(false);
  }, []);

  const mobileActions = !boardOpen ? (
    <span className={styles.mobilePanelButton}>
      <AstryxMoreMenu
        label="Wave actions"
        variant="ghost"
        size="lg"
        items={[
          ...(outlineItems.length > 0 ? [{
            label: 'Outline',
            onClick: () => {
              setMobilePanelKind('outline');
              setMobileCardMotion('forward');
              setMobileCardId(null);
              setMobilePanelOpen(true);
            },
          }] : []),
          {
            label: 'Cards',
            onClick: () => {
              setMobilePanelKind('cards');
              setMobileCardMotion('forward');
              setMobileCardId(null);
              setMobilePanelOpen(true);
            },
          },
          {
            label: 'Tasks',
            onClick: () => {
              setMobilePanelKind('tasks');
              setMobileCardMotion('forward');
              setMobileCardId(null);
              setMobilePanelOpen(true);
            },
          },
          {
            label: 'Conversations',
            onClick: () => {
              setMobilePanelKind('conversations');
              setMobileCardMotion('forward');
              setMobileCardId(null);
              setMobilePanelOpen(true);
            },
          },
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
            <h1 className={styles.titleHeading}><EditableTitle
              value={waveDisplayTitle(wave.title)}
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
          className={`${styles.panel} ${mobilePanelOpen ? styles.mobilePanelOpen : styles.mobilePanelClosed}`}
          data-nc-panel=""
          data-nc-mobile-page={mobilePanelOpen ? 'open' : 'closed'}
        >
          <div className={styles.mobileListSurface} aria-hidden={mobilePanelOpen ? undefined : true} inert={!mobilePanelOpen}>
            {mobilePanelOpen && (mobileCard !== undefined ? (
              <MobileListPage
                title={mobileCard.title ?? mobileCard.kind}
                backLabel="Cards"
                motion={mobileCardMotion}
                onBack={() => {
                  setMobileCardMotion('back');
                  setMobileCardId(null);
                }}
              >
                <dl className={styles.mobileCardFacts}>
                  <div><dt>Kind</dt><dd>{mobileCard.kind}</dd></div>
                  <div><dt>Ownership</dt><dd>{mobileCard.deletable ? 'User card' : 'Kernel-owned'}</dd></div>
                  <div><dt>Card ID</dt><dd>{mobileCard.id}</dd></div>
                </dl>
              </MobileListPage>
            ) : mobilePanelKind === 'outline' ? (
              <MobileListPage
                title="Outline"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={() => {
                  setMobileCardMotion('none');
                  setMobilePanelOpen(false);
                }}
              >
                <MobileList>
                  {outlineItems.flatMap((item) => [
                    <MobileListItem
                      key={item.blockId}
                      title={item.label}
                      meta={item.number === null ? undefined : String(item.number)}
                      onSelect={() => {
                        setMobileCardMotion('none');
                        setMobilePanelOpen(false);
                        onOpenOutline?.(item.blockId);
                      }}
                    />,
                    ...item.children.map((child) => (
                      <MobileListItem
                        key={child.blockId}
                        title={child.label}
                        nested
                        onSelect={() => {
                          setMobileCardMotion('none');
                          setMobilePanelOpen(false);
                          onOpenOutline?.(child.blockId);
                        }}
                      />
                    )),
                  ])}
                </MobileList>
              </MobileListPage>
            ) : mobilePanelKind === 'cards' ? (
              <MobileListPage
                title="Cards"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={() => {
                  setMobileCardMotion('none');
                  setMobilePanelOpen(false);
                }}
              >
                <MobileList>
                  {cards.map((card) => {
                    const label = card.title ?? card.kind;
                    return (
                      <MobileListItem
                        key={card.id}
                        title={label}
                        meta={card.kind}
                        onSelect={() => {
                          setMobileCardMotion('forward');
                          setMobileCardId(card.id);
                        }}
                      />
                    );
                  })}
                  {cards.length === 0 && <MobileListEmpty>No cards yet.</MobileListEmpty>}
                </MobileList>
              </MobileListPage>
            ) : mobilePanelKind === 'tasks' ? (
              <MobileListPage
                title="Tasks"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={() => {
                  setMobileCardMotion('none');
                  setMobilePanelOpen(false);
                }}
              >
                <MobileList>
                  {tasks.map((task) => (
                    <MobileListItem
                      key={task.blockId}
                      title={task.key}
                      meta={task.state === 'ready' ? 'Ready'
                        : task.state === 'withdrawn' ? 'Withdrawn'
                          : task.state === 'unreadable' ? 'Unreadable' : 'Not ready'}
                      onSelect={() => {
                        setMobileCardMotion('none');
                        setMobileCardId(null);
                        setMobilePanelOpen(false);
                        onOpenTask?.(task.blockId);
                      }}
                    />
                  ))}
                  {tasks.length === 0 && <MobileListEmpty>No tasks declared yet.</MobileListEmpty>}
                </MobileList>
              </MobileListPage>
            ) : (
              <MobileListPage
                title="Conversations"
                backLabel="Report"
                motion={mobileCardMotion}
                onBack={() => {
                  setMobileCardMotion('none');
                  setMobilePanelOpen(false);
                }}
              >
                <div className={styles.mobileConversationList}>
                  {conversationList ?? <p>No conversations yet.</p>}
                </div>
              </MobileListPage>
            ))}
          </div>
          <div className={styles.desktopPanelSurface} aria-hidden={mobilePanelOpen ? true : undefined} inert={mobilePanelOpen}>
          <PanelCard>
            <PanelModule title="Cards" action={cardsAction}>
              {cards.length === 0
                ? <PanelEmpty>No cards yet.</PanelEmpty>
                : (
                  <ul className={styles.cards} data-nc-card-inventory="">
                    {cards.map((card) => {
                      const title = card.title ?? null;
                      return (
                        <li key={card.id}>
                          <button
                            type="button"
                            className={styles.cardRow}
                            onClick={() => onOpenCard?.(card.id)}
                          >
                            <span className={styles.cardKind}>{title ?? card.kind}</span>
                            <span className={styles.cardMeta}>
                              {/* Only when a title took the name slot — an
                                  untitled card is already showing its kind
                                  there, and printing it twice is noise. */}
                              {title !== null && <span className={styles.cardKindTag}>{card.kind}</span>}
                              {!card.deletable && <span className={styles.kernelOwned}>kernel-owned</span>}
                            </span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                )}
            </PanelModule>
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
            */}
            <PanelModule title="Tasks">
              {tasks.length === 0
                ? <PanelEmpty>No tasks declared yet.</PanelEmpty>
                : (
                  <ul className={styles.tasks} data-nc-task-inventory="">
                    {tasks.map((task) => {
                      /* Bound before the JSX: narrowing a property access does
                         not survive into a click handler, and the alternative
                         is a cast that would outlive the check it stands in
                         for. */
                      const workerCardId = task.workerCardId;
                      return (
                      /*
                        ── Two controls in one row, not one control with two
                        destinations ───────────────────────────────────────────

                        The row used to *be* a `<button>`, and which of the two
                        landings it took was decided for the reader: a
                        dispatched task opened its worker card and everything
                        else revealed the block. That is one affordance
                        pretending to be two, and the cost fell on the case it
                        was meant to help — once a task was running, the panel
                        could no longer reach its declaration at all.

                        So the row is a plain `<li>` carrying two siblings, and
                        each says what it does:

                          - the row itself always reveals the block — the same
                            landing the outline, a `neige://` link and a
                            backlink all use;
                          - the *kind* (`terminal` / `codex` / `claude`) is the
                            card affordance, and only when there is a card to
                            open. `app/router` has already cleared
                            `workerCardId` for any card the **registry** cannot
                            draw, so such a row's kind renders as a label and
                            routes nowhere rather than bouncing the reader off
                            the URL.

                        A `<button>` may not nest inside a `<button>`, which is
                        the mechanical reason the row stopped being one; both
                        controls are still `<button>` + callback, and there is
                        still no `<a href>` here (INV-A11Y-061).

                        **"The row reveals the block" is enforced in CSS, not by
                        DOM containment.** Nesting is what would make it a DOM
                        fact, and nesting is the one thing forbidden here — so
                        the reveal button paints an invisible sheet over the
                        whole `<li>` (`.taskReveal::before`), and the two things
                        that must stay on top of it are the kind *button* and
                        the status dot. Everything else in the row is the reveal
                        control's own target: the kind's non-clickable `<span>`
                        form, and the trailing lane of a row that has no dot.
                        Both of those were dead zones, in a row whose whole
                        contract is that it is clickable, so the claim is
                        hit-tested in `task-row.browser.test.tsx` rather than
                        asserted here — jsdom has no layout and reports this
                        same tree whether the sheet covers the row or nothing.
                      */
                      <li key={task.blockId} className={styles.taskRow}>
                        <button
                          type="button"
                          className={styles.taskReveal}
                          title={`Show ${task.key} in the report`}
                          onClick={() => onOpenTask?.(task.blockId)}
                        >
                          {/* Mono: the key is the literal other reports and the
                              kernel address this task by (§2.2). */}
                          <span className={styles.taskKey}>{task.key}</span>
                          {/*
                            The declaration's own word — `Not ready`,
                            `Withdrawn`, `Unreadable` — and nothing else: the
                            run is the dot. A ready declaration is silent, and
                            so is one the kernel has since dispatched, because
                            `deriveReportTasks` drops the readiness word once a
                            status exists.

                            `Withdrawn` is struck through because the
                            declaration is struck through, and it cannot collide
                            with a run: a withdrawn row is never decorated with
                            one, so it keeps saying it was withdrawn even when
                            the task's `tasks` row outlived the withdrawal.
                          */}
                          {task.declaration !== null && (
                            <span className={task.state === 'withdrawn'
                              ? styles.taskWithdrawn
                              : styles.taskNote}
                            >
                              {task.declaration}
                            </span>
                          )}
                          {/*
                            ── The status, as a dot, and never as colour alone ──

                            Three carriers, not one: the accessible name spells
                            the status out, the form spells it out (hollow ring
                            / disc / square / ringed disc — see
                            `page.module.css`), and colour only reinforces
                            them. The palette alone could not do it: the four
                            semantic fills sit within 9 ΔL of one another in
                            light and 6 in dark, and dark's `--success` and
                            `--error` are the same lightness exactly.

                            `role="img"` + `aria-label` rather than a
                            visually-hidden span: the dot IS the graphic, so
                            naming it is what an accessible name is for, and the
                            label lands in the row button's own accessible name
                            (`bench-harness Status: running`) instead of adding
                            a second stop for a screen reader to walk past.
                            `title` carries the same fact to a sighted pointer,
                            which is what makes the colour a shorthand for a
                            word rather than the only carrier of it.

                            Both carry the kernel's *reason* too when it gave
                            one (#1147's `status_detail`), which is why the
                            hover is worth having at all on a failure: `failed`
                            alone is the thing the reader already sees, and
                            `failed — wave … is not a git repository` is the
                            answer they were about to go looking for.

                            It sits *inside* the reveal button on purpose: the
                            row's whole job is to reveal the block, and a
                            trailing target that silently did nothing would be a
                            hole in exactly the corner the eye is drawn to. That
                            is also what lets the dot own its own hover without
                            owning the click — being a DOM child is what makes
                            the click bubble, so the dot never needs
                            `pointer-events: none`, which is the change that
                            would take the hover away again. The target itself
                            is the row's whole trailing lane, not the 8px mark;
                            see `.taskDot::before` in `page.module.css` for why
                            the mark alone was unhoverable in practice.

                            `title` is the app's hover carrier everywhere else
                            (`ui/panel-card`, `app/shell/sidebar`,
                            `features/report/backlinks`) and there is no tooltip
                            primitive to reach for instead. Its limits are real
                            and are not papered over here: no touch, no keyboard
                            focus, a delay before it appears. What covers those
                            is the *other* carrier — `aria-label`, which folds
                            this same sentence into the row button's accessible
                            name, so focusing the row says it without a pointer
                            at all. Touch is the one seat left uncovered.

                            Its trailing position is CSS, not DOM order.
                          */}
                          {task.status !== null && (
                            <span
                              className={styles.taskDot}
                              data-nc-task-status={task.status}
                              role="img"
                              aria-label={`Status: ${taskStatusPhrase(task.status, task.statusDetail)}`}
                              title={taskStatusPhrase(task.status, task.statusDetail)}
                            />
                          )}
                        </button>
                        {/*
                          The kind is a word either way — what changes is
                          whether it is a control. `title` describes the
                          destination without touching the accessible name,
                          which stays the visible word (WCAG 2.5.3).
                        */}
                        {task.kind !== null && (workerCardId === null
                          ? <span className={styles.taskKind}>{task.kind}</span>
                          : (
                            <button
                              type="button"
                              className={styles.taskKindButton}
                              title={`Open the worker card for ${task.key}`}
                              onClick={() => onOpenCard?.(workerCardId)}
                            >
                              {task.kind}
                            </button>
                          ))}
                      </li>
                      );
                    })}
                  </ul>
                )}
            </PanelModule>
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
