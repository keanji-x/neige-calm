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

import { waveDisplayTitle, type CardWire, type Wave } from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { OperationFeedback, useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import styles from './page.module.css';

export type WavePageProps = Readonly<{
  wave: Wave;
  cards: readonly CardWire[];
  /** The panel card's second module, composed by `app/router` (features/chat). */
  /** The report document, composed by `app/router` (features/report). */
  report?: ReactNode;
  /** `REFERENCED BY` — omitted entirely when nothing cites this wave (§6.1). */
  backlinks?: ReactNode;
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** CR-8 — after a successful delete, focus lands on the cove page's title. */
  onRenameWave: (title: string) => void | Promise<void>;
  onDeleteWave: (signal: AbortSignal) => void | Promise<void>;
}>;


export function WavePage({
  wave, cards, report, backlinks, conversationList, conversationAction,
  onRenameWave, onDeleteWave,
}: WavePageProps) {
  const deletion = useDeleteConfirm((_id, signal) => onDeleteWave(signal));

  return (
    <section className={styles.page} data-nc-wave-page="">
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
      <PageHeader
        align="document"
        title={
          <h1 className={styles.titleHeading}><EditableTitle
            value={waveDisplayTitle(wave.title)}
            onCommit={onRenameWave}
            editLabel="Rename wave"
            inputLabel="Wave title"
            className={styles.title}
            isPageTitle
          /></h1>
        }
        meta={
          <>
            {/* Status is a different *kind* of thing from a title, so it is
                carried by shape and semantic colour, never by size — which is
                exactly why it can sit next to 18px type without competing. */}
            <WaveLifecycleBadge lifecycle={wave.lifecycle} />
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
            ×
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
        <aside className={styles.panel}>
          <PanelCard>
            <PanelModule title="Cards">
              {cards.length === 0
                ? <PanelEmpty>No cards yet.</PanelEmpty>
                : (
                  <ul className={styles.cards} data-nc-card-inventory="">
                    {cards.map((card) => (
                      <li key={card.id} className={styles.cardRow}>
                        <span className={styles.cardKind}>{card.title ?? card.kind}</span>
                        {!card.deletable && <span className={styles.kernelOwned}>kernel-owned</span>}
                      </li>
                    ))}
                  </ul>
                )}
            </PanelModule>
            {/*
              The wave's folder. It has been homeless since the page header lost
              its identity row: you choose it once at creation and then the app
              never showed it again. It is one line, in mono because the font is
              what says "literal" (§2.2), and it sits with CARDS because "what
              this wave is made of" and "where it works" are the same question.

              It is *not* a file browser. §8.3's FILES module was cut: a tree
              plus a viewer makes the document slot mean two different things,
              and a file has no block id, so every link, anchor and backlink in
              the app stops working the moment you open one. Evidence a report
              wants you to see belongs in the report, as a block.
            */}
            <PanelModule title="Folder">
              {wave.cwd === ''
                ? <PanelEmpty>No folder.</PanelEmpty>
                : <p className={styles.cwd} title={wave.cwd}>{wave.cwd}</p>}
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
