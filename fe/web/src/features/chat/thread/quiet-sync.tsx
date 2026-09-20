// The folded background turn: a planner turn that a report edit woke, drawn as one line the
// reader can open. The grouping is the domain's; this component draws the group it is handed.

import type { ReactNode } from 'react';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import {
  REPORT_EDIT_AUTHORS, type QuietSyncGroup, type QuietSyncOutcome, type ReportEditAuthor,
} from '../../../../../core/domain/conversation-quiet-sync.ts';
import styles from './quiet-sync.module.css';

/** The sentence per author. `satisfies` inside `Object.freeze` keeps the excess-property check a bare `Object.freeze({...})` would defeat. */
export const EDITED_BY: Readonly<Record<ReportEditAuthor, string>> = Object.freeze({
  user: 'You edited the report',
  assistant: 'The assistant edited the report',
  plugin: 'A plugin edited the report',
} satisfies Record<ReportEditAuthor, string>);

/** For a wake whose observation named no author. */
export const EDITED_BY_UNKNOWN = 'The report was edited';

export function quietSyncLine(author: ReportEditAuthor | null): string {
  return author !== null && REPORT_EDIT_AUTHORS.includes(author) ? EDITED_BY[author] : EDITED_BY_UNKNOWN;
}

/** The verdict per outcome, same register as the line. */
export const OUTCOME_LINE: Readonly<Record<QuietSyncOutcome, string>> = Object.freeze({
  accepted: 'accepted, no action',
  updated: 'updated the report',
} satisfies Record<QuietSyncOutcome, string>);

export type QuietSyncFoldProps = Readonly<{
  group: QuietSyncGroup;
  /** `HH:MM` of the wake, from the thread's own clock formatter. */
  time: string;
  /** True while this sync is the turn in flight: the live mark sits on the fold line, since the running activity is inside the closed disclosure. */
  live: boolean;
  /** The group's entries, already rendered by the thread. */
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
      data-nc-quiet-sync-outcome={group.outcome ?? undefined}
    >
      <summary className={styles.summary} title={`${line} · ${time}${verdict}`}>
        <span className={styles.disclosure} aria-hidden="true">›</span>
        <span className={styles.label} data-nc-quiet-sync-label="">
          Synced · {line} · <span className={styles.time}>{time}</span>{verdict}
        </span>
        {live && <ActivityIndicator state="working" />}
      </summary>
      <div className={styles.body} data-nc-quiet-sync-body="">
        {children}
      </div>
    </details>
  );
}
