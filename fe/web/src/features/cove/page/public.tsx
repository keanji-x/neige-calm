// The cove route, on the skeleton every route now shares: a one-row header, an
// unbounded document in the main column, and one rounded card top-right holding
// two modules — the wave list, then the conversation list.
//
// This overturns §8.2's "这一页*就是*一个列表". Picking a wave is still the
// page's first job and the list is still the card's first module; what changed
// is that a cove now has somewhere to put what comes *out* of the work, which
// it previously did not.
//
// Presentational by construction. It never fetches, never deletes, never
// navigates; every escape is a prop. In particular it does NOT render the wave
// list itself — `features/**` may not import a sibling feature domain, so
// `app/router` composes `<CovePage waveList={<WaveList …/>} />`.

import type { ReactNode } from 'react';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { deleteCoveCopy } from '../../../ui/confirm-dialog/copy.ts';
import { EditableTitle } from '../../../ui/editable-title/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import { OperationFeedback, useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { PanelAction, PanelCard, PanelModule } from '../../../ui/panel-card/public.tsx';
import { TypedDeleteBody, useTypedConfirm } from '../../../ui/typed-confirm/public.tsx';
import styles from './page.module.css';

export type CovePageProps = Readonly<{
  cove: Cove;
  waveCount: number;
  /** The wave list, composed by `app/router`. Owns the empty state too. */
  waveList: ReactNode;
  /** The panel card's second module, composed by `app/router` (features/chat). */
  /** The report document, composed by `app/router` (features/report). */
  report?: ReactNode;
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  onRenameCove: (name: string) => void | Promise<void>;
  onDeleteCove: (signal: AbortSignal) => void | Promise<void>;
  onRequestNewWave: () => void;
}>;

/**
 * INV-A11Y-061 — every affordance here is a `<button>` + callback. No `<a href>`
 * anywhere on this surface.
 *
 * INV-CONFIRM-001 — the destructive confirm owns the request for its lifetime:
 * Confirm goes busy (not really `disabled` — that would drop focus out of the
 * trap), Cancel stays enabled and aborts the request, and a `finally` clears
 * both flags so a *rejected* `onDeleteCove` cannot strand the dialog.
 */
export function CovePage({
  cove, waveCount, waveList, report, conversationList, conversationAction,
  onRenameCove, onDeleteCove, onRequestNewWave,
}: CovePageProps) {
  const deletion = useDeleteConfirm((_id, signal) => onDeleteCove(signal));
  const typed = useTypedConfirm(deletion.open ? cove.name : '');
  const copy = deleteCoveCopy(cove.name, waveCount);

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
        align="document"
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
          <h1 className={styles.titleHeading}><EditableTitle
            value={cove.name}
            onCommit={onRenameCove}
            editLabel="Rename cove"
            inputLabel="Cove name"
            className={styles.title}
            isPageTitle
          /></h1>
        }
        actions={
          /*
            One control. The `+` moved to the WAVES module head, next to the
            list it creates into — a page-level "new wave" and a list-level one
            are the same action, and two buttons with the same accessible name
            is a defect rather than redundancy.

            `title` gives the sighted hover label; `aria-label` gives the
            accessible name. §4.4 is explicit that a tooltip may not stand in
            for the accessible name — both are present, not either. That the
            glyph is unlabelled overrides §4.4's "每页只出现一次的动作必须带
            文字": the worry there is a row of glyphs becoming a memory test,
            and one `×` in its conventional meaning is not a row.
          */
          <button
            type="button"
            data-nc-role="icon"
            className={`${styles.headerAction} ${styles.headerDelete}`}
            aria-label={`Delete cove ${cove.name}`}
            title="Delete cove"
            onClick={() => deletion.request(cove.id)}
          >
            <Icon name="close" />
          </button>
        }
      />

      {/*
        The shared skeleton: an unbounded report document in the main column,
        one rounded card top-right with two modules.

        This overturns §8.2's "这一页*就是*一个列表". The list is still what you
        came for and it is still the card's first module, but a cove is a place
        work happens, and the page had nowhere to put what came *out* of that
        work. The main column is that place.

        No dashed box around it. A document is not a slot with edges — it is the
        column, and drawing a frame told you where a thing you cannot yet have
        would end, which is not information. §5.3's "render the shape" is still
        satisfied: the shape of a document is a full-width column of prose, and
        that is what an empty one looks like. What is left is one line of hint
        tone naming the one way to fill it.
      */}
      <div className={styles.content}>
        {/* The document itself comes from `features/report`, which is a sibling
            domain — so `app/router` composes it and this page only says where
            it goes. It owns its own empty state, because "empty" is a state of
            the document and not of the page around it. */}
        <div className={styles.doc}>{report}</div>

        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open: the drawer is a card on this exact track, and a
            panel left under it shows as a sliver along its edges. A local CSS
            Module class is not nameable from the shell's stylesheet, so the
            marker is the seam. */}
        <aside className={styles.panel} data-nc-panel="">
          <PanelCard>
            <PanelModule
              title="Waves"
              action={<PanelAction label="New wave" onClick={onRequestNewWave}><Icon name="plus" size="sm" /></PanelAction>}
            >
              {waveList}
            </PanelModule>
            <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
          </PanelCard>
        </aside>
      </div>

      {/* Deleting a cove cascades to every wave in it: the one operation in the
          product that earns a typed confirm (§4.3 / §6.13). The rail's entry
          point opens the same dialog with the same copy. */}
      <ConfirmDialog
        open={deletion.open}
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
        confirmState={deletion.pending ? 'busy' : (typed.matches ? 'ready' : 'blocked')}
        initialFocusRef={typed.inputRef}
        onConfirm={deletion.confirm}
        onCancel={deletion.cancel}
      />
      <OperationFeedback feedback={deletion.feedback} />
    </div>
  );
}
