// The `/wave/$waveId` surface.
//
// Presentational: the wave, its cove and its cards arrive as props, and every
// mutation and every navigation leaves through a callback — `features/**` may
// not import `app/**`, so the router owns both the fetch and the destination.
//
// INV-A11Y-061 — every navigation here is a `<button>` plus a callback. There
// is no `<a href>` anywhere on this page, and `public.contract.test.tsx` holds
// that line for the whole subtree.

import { type Cove } from '../../../../../core/domain/cove.ts';
import { waveDisplayTitle, type CardWire, type Wave } from '../../../../../core/domain/wave.ts';
import { DELETE_WAVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { WaveLifecycleBadge } from '../lifecycle-badge/public.tsx';
import styles from './page.module.css';

export type WavePageProps = Readonly<{
  wave: Wave;
  /** Absent when the wave points at a cove the current read cannot see. */
  cove: Cove | undefined;
  cards: readonly CardWire[];
  onOpenCove: () => void;
  onOpenToday: () => void;
  onRenameWave: (title: string) => void | Promise<void>;
  onDeleteWave: () => void | Promise<void>;
}>;

const UNKNOWN_COVE_LABEL = 'Unknown cove';

/**
 * The card runtime — terminals, editors, the draggable board — is a later
 * slice. Until it lands the page still has to answer "what is in this wave",
 * so the body is an honest inventory rather than an empty panel or a
 * half-built renderer that would have to be thrown away.
 */
const CARD_RUNTIME_NOTE = 'Card runtime lands in a later slice.';

export function WavePage({
  wave, cove, cards, onOpenCove, onOpenToday, onRenameWave, onDeleteWave,
}: WavePageProps) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const coveName = cove?.name ?? UNKNOWN_COVE_LABEL;

  /**
   * The confirm stays mounted for the whole round trip — Confirm disabled,
   * Cancel still live — and `finally` clears both flags. A rejected
   * `onDeleteWave` must not strand the dialog open around a dead Confirm
   * button, which is what a `then`-only reset would do.
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
      <header className={styles.header}>
        <nav className={styles.crumbs} aria-label="Breadcrumb">
          <button type="button" className={styles.back} aria-label="Back to cove" onClick={onOpenCove}>←</button>
          <button type="button" className={styles.crumb} onClick={onOpenToday}>Today</button>
          <span className={styles.crumbSeparator} aria-hidden="true">/</span>
          <button type="button" className={styles.crumb} onClick={onOpenCove}>
            <span className={styles.coveDot} style={{ background: cove?.color }} aria-hidden="true" />
            {coveName}
          </button>
        </nav>

        <div className={styles.titleRow}>
          <EditableTitle
            value={waveDisplayTitle(wave.title)}
            onCommit={onRenameWave}
            editLabel="Rename wave"
            inputLabel="Wave title"
            className={styles.title}
          />
          <WaveLifecycleBadge lifecycle={wave.lifecycle} />
          <button
            type="button"
            className={styles.delete}
            onClick={() => setConfirmOpen(true)}
          >
            Delete
          </button>
        </div>

        {wave.cwd !== '' && <p className={styles.cwd}>{wave.cwd}</p>}
      </header>

      <div className={styles.body}>
        <h2 className={styles.sectionTitle}>Cards</h2>
        {cards.length === 0
          ? <p className={styles.empty}>This wave has no cards yet. {CARD_RUNTIME_NOTE}</p>
          : (
            <ul className={styles.cards} data-nc-card-inventory="">
              {cards.map((card) => (
                <li key={card.id} className={styles.card}>
                  <span className={styles.cardKind}>{card.kind}</span>
                  <span className={styles.cardTitle}>{card.title ?? card.kind}</span>
                  {!card.deletable && <span className={styles.kernelOwned}>kernel-owned</span>}
                  <span className={styles.cardNote}>{CARD_RUNTIME_NOTE}</span>
                </li>
              ))}
            </ul>
          )}
      </div>

      <ConfirmDialog
        open={confirmOpen}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmDisabled={deleting}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </section>
  );
}
