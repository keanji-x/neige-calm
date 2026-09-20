// `REFERENCED BY` — who cites this track: one row per citing track, the count when there is more
// than one, and the citing sentence as the row's tooltip. `truncated` and `skipped_sources` are rendered, not dropped.

import type { TrackBacklink, TrackBacklinks } from '../../../../../core/domain/report.ts';
import { groupBacklinks } from '../../../../../core/domain/report.ts';
import styles from './backlinks.module.css';

export type ReportBacklinksProps = Readonly<{
  trackId: string;
  backlinks: TrackBacklinks;
  /** Open the citing track, landing on the block the citation is written in. */
  onOpen: (trackId: string, blockId: string) => void;
}>;

/** The sentence a citation is written in, flattened for a `title` attribute. */
function quoteText(backlink: TrackBacklink): string {
  const quote = backlink.quote;
  if (quote === null || quote === undefined) return backlink.label;
  return `${quote.head_elided ? '…' : ''}${quote.before}${quote.label}${quote.after}${quote.tail_elided ? '…' : ''}`;
}

/** The distinct sentences a track cites you from, deduped by source block: two links in one paragraph are two backlinks with near-identical quotes. */
function mentions(entries: readonly TrackBacklink[]): string[] {
  const seen = new Set<string>();
  return entries.flatMap((entry) => {
    if (seen.has(entry.src_block_id)) return [];
    seen.add(entry.src_block_id);
    return [quoteText(entry)];
  });
}

export function ReportBacklinks({ trackId, backlinks, onOpen }: ReportBacklinksProps) {
  const groups = groupBacklinks(backlinks.backlinks, trackId);

  return (
    <div className={styles.backlinks}>
      <ul>
        {groups.map((group) => {
          const quotes = mentions(group.entries);
          return (
            <li key={group.trackId}>
              <button
                type="button"
                className={styles.row}
                title={quotes.join('\n\n')}
                onClick={() => onOpen(group.trackId, group.entries[0]?.src_block_id ?? '')}
              >
                <span className={styles.title}>{group.title}</span>
                {quotes.length > 1 && <span className={styles.count}>{quotes.length}</span>}
              </button>
            </li>
          );
        })}
      </ul>
      {backlinks.truncated && (
        <p className={styles.note} role="status">Some backlinks are not shown.</p>
      )}
      {backlinks.skipped_sources > 0 && (
        <p className={styles.note} role="status">
          {backlinks.skipped_sources} source report
          {backlinks.skipped_sources === 1 ? '' : 's'} could not be read.
        </p>
      )}
    </div>
  );
}
