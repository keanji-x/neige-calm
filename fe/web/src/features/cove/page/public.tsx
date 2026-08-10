// The cove route shell: header (swatch, rename, wave count, new-wave, delete)
// plus a body slot for the wave list.
//
// Presentational by construction. It never fetches, never deletes, never
// navigates; every escape is a prop. In particular it does NOT render the wave
// list itself — `features/**` may not import a sibling feature domain, so
// `app/router` composes `<CovePage waveList={<WaveList …/>} />`.

import type { ReactNode } from 'react';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { DELETE_COVE_COPY } from '../../../ui/confirm-dialog/copy.ts';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './page.module.css';

export type CovePageProps = Readonly<{
  cove: Cove;
  waveCount: number;
  /** The wave list, composed by `app/router`. Owns the empty state too. */
  waveList: ReactNode;
  onRenameCove: (name: string) => void | Promise<void>;
  onDeleteCove: () => void | Promise<void>;
  onRequestNewWave: () => void;
}>;

/**
 * INV-A11Y-061 — every affordance here is a `<button>` + callback. No `<a href>`
 * anywhere on this surface.
 *
 * INV-CONFIRM-001 — the destructive confirm stays mounted for the whole await:
 * Confirm goes disabled, Cancel stays enabled (the user must keep an exit), and
 * a `finally` clears both pending and open so a *rejected* `onDeleteCove`
 * cannot strand the dialog in a permanently-disabled state.
 */
export function CovePage({
  cove, waveCount, waveList, onRenameCove, onDeleteCove, onRequestNewWave,
}: CovePageProps) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const confirmDelete = () => {
    if (deleting) return;
    setDeleting(true);
    void (async () => {
      try {
        await onDeleteCove();
      } catch {
        // The caller owns surfacing the failure; this surface only has to make
        // sure the dialog does not strand. See INV-CONFIRM-001.
      } finally {
        setDeleting(false);
        setConfirmOpen(false);
      }
    })();
  };

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <span
          className={styles.swatch}
          data-nc-cove-swatch
          style={{ background: cove.color }}
          aria-hidden="true"
        />
        <EditableTitle
          value={cove.name}
          onCommit={onRenameCove}
          editLabel="Rename cove"
          inputLabel="Cove name"
          className={styles.title}
        />
        <span className={styles.count} data-nc-wave-count>
          {waveCount} {waveCount === 1 ? 'wave' : 'waves'}
        </span>
        <div className={styles.actions}>
          <button type="button" className={styles.newWave} onClick={onRequestNewWave}>
            + New wave
          </button>
          <button
            type="button"
            className={styles.delete}
            aria-label={`Delete cove ${cove.name}`}
            onClick={() => setConfirmOpen(true)}
          >
            Delete
          </button>
        </div>
      </header>

      <div className={styles.body}>{waveList}</div>

      <ConfirmDialog
        open={confirmOpen}
        title={DELETE_COVE_COPY.title}
        description={DELETE_COVE_COPY.description}
        confirmLabel={DELETE_COVE_COPY.confirmLabel}
        confirmDisabled={deleting}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </div>
  );
}
