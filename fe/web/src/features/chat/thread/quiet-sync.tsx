// #1667 D3 — the folded background turn.
//
// A planner turn that a report edit woke is a sync, not a reply: the planner
// reads a block diff, reconciles, and — if the prompt is doing its job — says
// nothing. Drawn as an ordinary exchange, that turn was a system line, a few
// activity lines and a "re-read the report" bubble down the column every
// time the reader saved. It is drawn here instead as one line the reader can
// open: who edited, when, and a disclosure. The whole turn is still there
// under it, verbatim, because the fold is about attention, not about
// hiding what the agent did.
//
// The line reads as a system observation on purpose — same type, same ink,
// same disclosure glyph as the kernel's other system entries — and not as a
// speaker: nobody in the conversation said "synced". The only thing that
// distinguishes it from the plain system disclosure is that it swallows what
// followed, and that is decided in `core/domain/conversation-quiet-sync.ts`,
// not here; this component draws a group it is handed.
//
// **What the planner says out loud stays out, and so does a sync that ended
// badly.** A `calm.user.notify` turn and a `failed` / `interrupted` outcome
// are lifted out of the group before it reaches this component and drawn by
// the transcript after the fold, as a normal agent bubble and as the red
// outcome line — see `foldQuietSyncs`.
//
// **What the sync did (#1678 A4).** A closed fold looks the same whether the
// edit was taken in or missed, so once the turn has completed the line ends
// with the verdict the domain computed (`QuietSyncGroup.outcome`): "accepted,
// no action" or "updated the report". Nothing is appended while the turn is
// running (the live mark is the state), after a bad ending (the lifted red
// line says it) or when the planner spoke (the lifted bubble says it).
//
// One component for every width. The phone gets the same `<details>`: a
// native disclosure needs no pointer geometry, and the line is short enough
// to fit a 360px column at `--text-xs` without a second row; the label clips
// with an ellipsis where the verdict does not fit, and the `title` carries it.

import type { ReactNode } from 'react';

import {
  REPORT_EDIT_AUTHORS, type QuietSyncGroup, type QuietSyncOutcome, type ReportEditAuthor,
} from '../../../../../core/domain/conversation-quiet-sync.ts';
import styles from './quiet-sync.module.css';

/**
 * The sentence per author. `satisfies` inside `Object.freeze` keeps the
 * excess-property check that a bare `Object.freeze({...})` would defeat, and
 * the `Record` keeps a missing author a type error; `quiet-sync.test.tsx`
 * pins both directions at runtime as well.
 */
export const EDITED_BY: Readonly<Record<ReportEditAuthor, string>> = Object.freeze({
  user: 'You edited the report',
  assistant: 'The assistant edited the report',
  plugin: 'A plugin edited the report',
} satisfies Record<ReportEditAuthor, string>);

/** For a wake whose observation named no author (pre-#1252 rows). */
export const EDITED_BY_UNKNOWN = 'The report was edited';

export function quietSyncLine(author: ReportEditAuthor | null): string {
  return author !== null && REPORT_EDIT_AUTHORS.includes(author) ? EDITED_BY[author] : EDITED_BY_UNKNOWN;
}

/** The verdict per outcome, same register as the line; pinned both ways in
 *  `quiet-sync.test.tsx` like `EDITED_BY`. */
export const OUTCOME_LINE: Readonly<Record<QuietSyncOutcome, string>> = Object.freeze({
  accepted: 'accepted, no action',
  updated: 'updated the report',
} satisfies Record<QuietSyncOutcome, string>);

export type QuietSyncFoldProps = Readonly<{
  group: QuietSyncGroup;
  /** `HH:MM` of the wake, from the thread's own clock formatter. */
  time: string;
  /** True while this sync is the turn in flight: the live mark sits on the
   *  fold line, because the running activity that would carry it is inside
   *  the closed disclosure. */
  live: boolean;
  /** The group's entries, already rendered by the thread — the fold owns the
   *  line, not the drawing of what is under it. */
  children: ReactNode;
}>;

export function QuietSyncFold({ group, time, live, children }: QuietSyncFoldProps) {
  const line = quietSyncLine(group.author);
  const verdict = group.outcome === null ? '' : ` · ${OUTCOME_LINE[group.outcome]}`;
  return (
    <details
      className={styles.fold}
      data-nc-turn="quiet-sync"
      data-nc-quiet-sync-author={group.author ?? 'unknown'}
      data-nc-quiet-sync-outcome={group.outcome ?? 'none'}
    >
      <summary className={styles.summary} title={`${line} · ${time}${verdict}`}>
        <span className={styles.disclosure} aria-hidden="true">›</span>
        <span className={styles.label} data-nc-quiet-sync-label="">
          Synced · {line} · <span className={styles.time}>{time}</span>{verdict}
        </span>
        {live && <span className={styles.live} aria-label="Working" />}
      </summary>
      <div className={styles.body} data-nc-quiet-sync-body="">
        {children}
      </div>
    </details>
  );
}
