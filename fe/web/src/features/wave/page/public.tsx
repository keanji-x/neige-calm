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

import type { ReactNode } from 'react';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { waveDisplayTitle, type CardWire, type Wave, type WaveLifecycle } from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { OperationFeedback, useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import styles from './page.module.css';

export type WavePageProps = Readonly<{
  wave: Wave;
  cards: readonly CardWire[];
  /**
   * The wave's declared tasks, derived by `app/router` from the report's own
   * `task` blocks (`core/domain/report`'s `deriveReportTasks`) — not a second
   * source of truth, and deliberately statusless: see the note there.
   */
  tasks: readonly ReportTaskRow[];
  /** The panel card's second module, composed by `app/router` (features/chat). */
  /** The report document, composed by `app/router` (features/report). */
  report?: ReactNode;
  /** `REFERENCED BY` — omitted entirely when nothing cites this wave (§6.1). */
  backlinks?: ReactNode;
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** The Cards module head's `+`, composed by `app/router`. */
  cardsAction?: ReactNode;
  onOpenCard?: (cardId: string) => void;
  /** Reveal a task's block in the document — the same landing the outline, a
   *  `neige://` link and a backlink all use. */
  onOpenTask?: (blockId: string) => void;
  /**
   * The card grid, composed by `app/router`. Positioned over the document
   * column so the page title stays the only title bar — a second overlay
   * header was overflowing the measure.
   */
  board?: ReactNode;
  onCloseBoard?: () => void;
  /** CR-8 — after a successful delete, focus lands on the cove page's title. */
  onRenameWave: (title: string) => void | Promise<void>;
  onDeleteWave: (signal: AbortSignal) => void | Promise<void>;
}>;


/** Draft / done / canceled are idle facts; they do not earn a header badge. */
function headerLifecycle(lifecycle: WaveLifecycle): WaveLifecycle | null {
  if (lifecycle === 'draft' || lifecycle === 'done' || lifecycle === 'canceled') return null;
  return lifecycle;
}

export function WavePage({
  wave, cards, tasks, report, backlinks, conversationList, conversationAction,
  cardsAction, onOpenCard, onOpenTask, board, onCloseBoard, onRenameWave, onDeleteWave,
}: WavePageProps) {
  const deletion = useDeleteConfirm((_id, signal) => onDeleteWave(signal));
  const boardOpen = onCloseBoard !== undefined;
  const lifecycle = headerLifecycle(wave.lifecycle);

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
        actions={
          // This page has no primary action, and that is legal and common:
          // creating a card is a gesture on the board, renaming is in place.
          // Same icon + tooltip treatment as the cove page — one destructive
          // affordance, one glyph, wherever it appears.
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
        }
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
          `kind` is a card's identity and `title` its label, so a card with a
          title shows the title alone rather than printing both in a 308px
          column. FOLDER sits with it because a wave's cwd is a fact about the
          same object; it left the page header, where it was the largest fixed
          cost the page paid for its least-read fact.
        */}
        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open — see the same marker on the cove page. */}
        <aside className={styles.panel} data-nc-panel="">
          <PanelCard>
            <PanelModule title="Cards" action={cardsAction}>
              {cards.length === 0
                ? <PanelEmpty>No cards yet.</PanelEmpty>
                : (
                  <ul className={styles.cards} data-nc-card-inventory="">
                    {cards.map((card) => {
                      const label = card.title ?? card.kind;
                      return (
                        <li key={card.id}>
                          <button
                            type="button"
                            className={styles.cardRow}
                            onClick={() => onOpenCard?.(card.id)}
                          >
                            <span className={styles.cardKind}>{label}</span>
                            {!card.deletable && <span className={styles.kernelOwned}>kernel-owned</span>}
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
                    {tasks.map((task) => (
                      <li key={task.blockId}>
                        <button
                          type="button"
                          className={styles.taskRow}
                          onClick={() => onOpenTask?.(task.blockId)}
                        >
                          {/* Mono: the key is the literal other reports and the
                              kernel address this task by (§2.2). */}
                          <span className={styles.taskKey}>{task.key}</span>
                          {/* Only `ready` is silent. A task the agent has not
                              finished declaring, and one it withdrew, are both
                              things the reader would otherwise have to open the
                              document to discover; a task that is ready is the
                              ordinary case and gets no word for it. */}
                          {task.state !== 'ready' && (
                            <span className={task.state === 'withdrawn' ? styles.taskWithdrawn : styles.taskNotReady}>
                              {task.state === 'withdrawn' ? 'Withdrawn' : 'Not ready'}
                            </span>
                          )}
                        </button>
                      </li>
                    ))}
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
        </aside>
      </div>
      {board}
      </div>

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
