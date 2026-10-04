// Report change evidence uses the same disclosure and document renderer as Reference.
import type { ReactNode } from 'react';
import type { ReportChange, ReportEditEntry } from '../../../../../core/domain/daily-planner.ts';
import { Button } from '@astryxdesign/core/Button';
import { ReportDetails } from '../document/details.tsx';
import { ReportDocument } from '../document/public.tsx';
import styles from './changes.module.css';

export function ReportChangeDetails({ change, onOpenTrack, onToggleEdits, children }: Readonly<{
  change: ReportChange; onOpenTrack: (trackId: string) => void;
  onToggleEdits: (open: boolean) => void; children: ReactNode;
}>) {
  return <ReportDetails title={change.track_title || 'Untitled Track'}
    meta={`${change.area_name} · ${change.edit_count} report ${change.edit_count === 1 ? 'edit' : 'edits'}`}
    layout="grid" variant="entry">
    <div className={styles.content}>
      <Button label="Open source report" variant="ghost" size="sm" onClick={() => onOpenTrack(change.track_id)}>Open report</Button>
      {change.summary_before !== change.summary_after && <dl className={styles.fields}>
        <dt>Previous summary</dt><dd>{change.summary_before || '—'}</dd>
        <dt>Current summary</dt><dd>{change.summary_after || '—'}</dd>
      </dl>}
      <ReportDetails title="Body changes" layout="grid" variant="entry">
        {change.patch === '' ? <p className={styles.note}>No net body change; individual edits retain the history.</p>
          : <pre className={styles.patch}><code>{change.patch}</code></pre>}
        {change.patch_truncated && <p className={styles.note}>Patch shortened. Read individual edits for the full content.</p>}
      </ReportDetails>
      <ReportDetails title="Individual edits" layout="grid" variant="entry" onToggle={onToggleEdits}>{children}</ReportDetails>
    </div>
  </ReportDetails>;
}

export function ReportEditDetails({ entry, timeZone, onOpenTrack }: Readonly<{
  entry: ReportEditEntry; timeZone: string; onOpenTrack: (trackId: string) => void;
}>) {
  const time = new Date(entry.at).toLocaleTimeString('en-GB', { timeZone, hour: '2-digit', minute: '2-digit', second: '2-digit' });
  return <ReportDetails title="Report edit" meta={time} layout="grid" variant="entry">
    <div className={styles.content}>
      <ReportDetails title="After" layout="grid" variant="entry">
        <ReportDocument report={{ summary: entry.edit.summary_after, body: entry.edit.body_after, blocks: null }} empty={null}
          onOpenLink={(target) => onOpenTrack(target.trackId)} />
      </ReportDetails>
      <ReportDetails title="Before" layout="grid" variant="entry">
        <ReportDocument report={{ summary: entry.edit.summary_before, body: entry.edit.body_before, blocks: null }} empty={null}
          onOpenLink={(target) => onOpenTrack(target.trackId)} />
      </ReportDetails>
    </div>
  </ReportDetails>;
}
