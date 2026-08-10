// The cove route: a one-row header (rename in place, new-wave, delete) over a
// body slot for the wave list. Everything else this header once carried — an
// identity dot, a wave count, a derived cwd — is gone; see the notes inline.
//
// §8.2 — this page *is* a list. Everything else on it is that list's label, and
// the page's whole job is "pick the wave I need, or start one".
//
// Presentational by construction. It never fetches, never deletes, never
// navigates; every escape is a prop. In particular it does NOT render the wave
// list itself — `features/**` may not import a sibling feature domain, so
// `app/router` composes `<CovePage waveList={<WaveList …/>} />`.

import type { ReactNode, RefObject } from 'react';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { deleteCoveCopy } from '../../../ui/confirm-dialog/copy.ts';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { TypedDeleteBody, useTypedConfirm } from '../../../ui/typed-confirm/public.tsx';
import styles from './page.module.css';

export type CovePageProps = Readonly<{
  cove: Cove;
  waveCount: number;
  /** The wave list, composed by `app/router`. Owns the empty state too. */
  waveList: ReactNode;
  /** CR-8 — after a successful delete, focus lands on the next page's title. */
  pageTitleRef?: RefObject<HTMLElement | null>;
  onRenameCove: (name: string) => void | Promise<void>;
  onDeleteCove: () => void | Promise<void>;
  onRequestNewWave: () => void;
}>;

/**
 * INV-A11Y-061 — every affordance here is a `<button>` + callback. No `<a href>`
 * anywhere on this surface.
 *
 * INV-CONFIRM-001 — the destructive confirm stays mounted for the whole await:
 * Confirm goes busy (not really `disabled` — that would drop focus out of the
 * trap), Cancel stays enabled so the user always keeps an exit, and a `finally`
 * clears both flags so a *rejected* `onDeleteCove` cannot strand the dialog.
 */
export function CovePage({
  cove, waveCount, waveList, pageTitleRef, onRenameCove, onDeleteCove, onRequestNewWave,
}: CovePageProps) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const typed = useTypedConfirm(confirmOpen ? cove.name : '');
  const copy = deleteCoveCopy(cove.name, waveCount);

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
      {/*
        One row. A cove has no domain ancestor, so the breadcrumb row is omitted
        by rule (§6.4) — where it sits is what the rail is for — and the cwd row
        is gone with it, so `--header-h` is 32.

        The cwd was not a cove's fact. A cove has no `cwd` column; this page
        synthesised one by asking whether every wave inside happened to agree,
        and printed the answer as though it were an attribute of the cove. That
        is the same defect as the wave count, one layer down: a derived number
        dressed as a stored one. Worse, it was unstable — adding one wave in
        another folder made the cove's "identity" vanish. A wave's cwd is real
        (the agent literally runs there) and stays on the wave page.
      */}
      <PageHeader
        /*
         * No identity dot and no wave count.
         *
         * The dot was the only colour on the page, and it was restating the
         * name directly beside it — on a route whose whole content is a quiet
         * list, that made the loudest pixel the least informative one. It still
         * earns its place where rows genuinely span coves: Today's agenda and
         * the calendar day dot.
         *
         * The count answered a question nobody asks. You open a cove to pick a
         * wave, not to learn how many there are, and the list below already
         * says it — at a glance, and with the names attached.
         */
        title={
          <EditableTitle
            value={cove.name}
            onCommit={onRenameCove}
            editLabel="Rename cove"
            inputLabel="Cove name"
            className={styles.title}
            isPageTitle
          />
        }
        actions={
          <>
            {/*
              Icon buttons with a hover tooltip, not labelled buttons. This
              overrides §4.4 ("每页只出现一次的动作必须带文字"), whose worry is
              that a row of unlabelled glyphs becomes a memory test — with two
              controls, a `+` and an `×` in their conventional meanings, and a
              tooltip on each, that worry does not apply, and a 94px solid
              accent slab was the loudest thing on a page whose content is a
              quiet list.

              `title` gives the sighted hover label; `aria-label` gives the
              accessible name. §4.4 is explicit that the tooltip may not stand
              in for the accessible name — both are present, not either.
            */}
            <button
              type="button"
              data-nc-role="icon"
              className={styles.headerAction}
              aria-label="New wave"
              title="New wave"
              onClick={onRequestNewWave}
            >
              +
            </button>
            <button
              type="button"
              data-nc-role="icon"
              className={`${styles.headerAction} ${styles.headerDelete}`}
              aria-label={`Delete cove ${cove.name}`}
              title="Delete cove"
              onClick={() => setConfirmOpen(true)}
            >
              ×
            </button>
          </>
        }
      />

      <div className={styles.body}>{waveList}</div>

      {/* Deleting a cove cascades to every wave in it: the one operation in the
          product that earns a typed confirm (§4.3 / §6.13). The rail's entry
          point opens the same dialog with the same copy. */}
      <ConfirmDialog
        open={confirmOpen}
        title={copy.title}
        description={<TypedDeleteBody
          copy={copy}
          expected={cove.name}
          value={typed.value}
          inputRef={typed.inputRef}
          onChange={typed.setValue}
        />}
        confirmLabel={copy.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={deleting ? 'busy' : (typed.matches ? 'ready' : 'blocked')}
        initialFocusRef={typed.inputRef}
        restoreFocusRef={pageTitleRef}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmOpen(false)}
      />
    </div>
  );
}
