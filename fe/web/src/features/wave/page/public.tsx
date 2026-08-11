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

import type { ReactNode, RefObject } from 'react';

import { coveSlotVar, type Cove } from '../../../../../core/domain/cove.ts';
import { waveDisplayTitle, type CardWire, type Wave } from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { Breadcrumb, PageHeader } from '../../../ui/page-header/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import styles from './page.module.css';

export type WavePageProps = Readonly<{
  wave: Wave;
  /** Absent when the wave points at a cove the current read cannot see. */
  cove: Cove | undefined;
  cards: readonly CardWire[];
  /** The panel card's second module, composed by `app/router` (features/chat). */
  conversationList?: ReactNode;
  /** CR-8 — after a successful delete, focus lands on the cove page's title. */
  pageTitleRef?: RefObject<HTMLElement | null>;
  onOpenCove: () => void;
  onOpenToday: () => void;
  onRenameWave: (title: string) => void | Promise<void>;
  onDeleteWave: () => void | Promise<void>;
}>;

const UNKNOWN_COVE_LABEL = 'Unknown cove';

export function WavePage({
  wave, cove, cards, conversationList, pageTitleRef,
  onOpenCove, onOpenToday, onRenameWave, onDeleteWave,
}: WavePageProps) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const coveName = cove?.name ?? UNKNOWN_COVE_LABEL;

  /**
   * The confirm stays mounted for the whole round trip — Confirm busy, Cancel
   * still live — and `finally` clears both flags. A rejected `onDeleteWave`
   * must not strand the dialog open around a dead Confirm button, which is what
   * a `then`-only reset would do.
   *
   * Deleting a wave is a plain confirm, not a typed one: it is not the
   * catastrophic, cascading operation that earns the third rung (§4.3).
   */
  const confirmDelete = () => {
    setDeleting(true);
    void Promise.resolve()
      .then(() => onDeleteWave())
      .catch(() => {
        // The caller owns error surfacing; the dialog's job is to get out of the way.
      })
      .finally(() => {
        setDeleting(false);
        setConfirmOpen(false);
      });
  };

  return (
    <section className={styles.page} data-nc-wave-page="">
      {/* All three rows: the only route in the app whose --header-h is 92. */}
      <PageHeader
        breadcrumb={
          <Breadcrumb
            ancestor="Today"
            onNavigate={onOpenToday}
            onBack={onOpenCove}
            onNavigateCurrent={onOpenCove}
            backLabel="Back to cove"
            current={
              <>
                <span
                  className={styles.coveDot}
                  style={cove === undefined ? undefined : { background: `var(${coveSlotVar(cove.id)})` }}
                  aria-hidden="true"
                />
                {coveName}
              </>
            }
          />
        }
        title={
          <EditableTitle
            value={waveDisplayTitle(wave.title)}
            onCommit={onRenameWave}
            editLabel="Rename wave"
            inputLabel="Wave title"
            className={styles.title}
            isPageTitle
          />
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
            onClick={() => setConfirmOpen(true)}
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
        <div className={styles.doc}>
          {/*
            No report slot. A wave's report is not a thing this page is waiting
            to be handed — it is written *by* the conversation, so a framed
            placeholder promising one was describing a feature rather than a
            state. What the column owes an empty wave is the same thing an empty
            cove is owed: one line naming the one way to fill it.
          */}
          <p className={styles.docEmpty}>
            Nothing written here yet. Start a conversation to add something.
          </p>
        </div>

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
              {/* The folder is a fact about the same object, so it rides in
                  this module rather than earning a third one — but it is a
                  different *kind* of fact from a card, so a hairline separates
                  them (boundary ladder step ②). Without it the path read as a
                  second line of the empty state. */}
              {wave.cwd !== '' && (
                <p className={styles.cwd}>
                  <span className={styles.cwdLabel}>Folder</span>
                  {wave.cwd}
                </p>
              )}
            </PanelModule>
            <PanelModule title="Conversations">{conversationList}</PanelModule>
          </PanelCard>
        </aside>
      </div>

      <ConfirmDialog
        open={confirmOpen}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={deleting ? 'busy' : 'ready'}
        restoreFocusRef={pageTitleRef}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </section>
  );
}
