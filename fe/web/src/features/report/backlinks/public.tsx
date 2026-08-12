// `REFERENCED BY` — who cites this wave (§8.3).
//
// It is a module inside the panel card, not a rail section and not a page of
// its own: "who is using this" is a fact *about this wave*, and it belongs
// next to the other facts about it. §6.5 forbids a card inside a card, so the
// modules share one card and are separated by hairlines.
//
// **One row per citing wave — its title, and nothing else.**
//
// The kernel answers per *link*, not per wave, and a report that cites you
// twice in one sentence produces two entries whose quotes are two overlapping
// slices of that sentence. Rendering them as written printed the same words
// twice, one under the other, in the app's narrowest column. Grouping by wave
// alone did not fix it — the duplication is *inside* a group.
//
// So the row is the title, on one line, and the count of citations when there
// is more than one. The sentence a citation is written in is still worth
// having, but not at three lines a piece in a 280 column: it rides along as
// the row's tooltip, where it costs nothing until it is asked for.
//
// A knowingly incomplete list still says so. `truncated` and `skipped_sources`
// are rendered rather than dropped: a citation list that is quietly short is
// worse than one that admits it is short.

import type { WaveBacklink, WaveBacklinks } from '../../../../../core/domain/report.ts';
import { groupBacklinks } from '../../../../../core/domain/report.ts';
import styles from './backlinks.module.css';

export type ReportBacklinksProps = Readonly<{
  waveId: string;
  backlinks: WaveBacklinks;
  /** Open the citing wave, landing on the block the citation is written in. */
  onOpen: (waveId: string, blockId: string) => void;
}>;

/** The sentence a citation is written in, flattened for a `title` attribute. */
function quoteText(backlink: WaveBacklink): string {
  const quote = backlink.quote;
  if (quote === null || quote === undefined) return backlink.label;
  return `${quote.head_elided ? '…' : ''}${quote.before}${quote.label}${quote.after}${quote.tail_elided ? '…' : ''}`;
}

/**
 * The distinct sentences a wave cites you from.
 *
 * Two links in one paragraph are two backlinks with near-identical quotes, so
 * the tooltip dedupes by *source block* — the unit a reader would call "one
 * mention" — and not by link.
 */
function mentions(entries: readonly WaveBacklink[]): string[] {
  const seen = new Set<string>();
  return entries.flatMap((entry) => {
    if (seen.has(entry.src_block_id)) return [];
    seen.add(entry.src_block_id);
    return [quoteText(entry)];
  });
}

export function ReportBacklinks({ waveId, backlinks, onOpen }: ReportBacklinksProps) {
  const groups = groupBacklinks(backlinks.backlinks, waveId);

  return (
    <div className={styles.backlinks}>
      <ul>
        {groups.map((group) => {
          const quotes = mentions(group.entries);
          return (
            <li key={group.waveId}>
              {/* INV-A11Y-061: a button and a callback, like every other
                  navigation in the app. */}
              <button
                type="button"
                className={styles.row}
                title={quotes.join('\n\n')}
                onClick={() => onOpen(group.waveId, group.entries[0]?.src_block_id ?? '')}
              >
                <span className={styles.title}>{group.title}</span>
                {/* The count only appears when it says something. "1" next to
                    every row would be a column of ones. */}
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
