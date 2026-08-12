// `REFERENCED BY` — who cites this wave (§8.3).
//
// It is a module inside the panel card, not a rail section and not a page of
// its own: "who is using this" is a fact *about this wave*, and it belongs
// next to the other facts about it. §6.5 forbids a card inside a card, so the
// four modules share one card and are separated by hairlines.
//
// **A backlink without its quote is worth very little.** The thing you want to
// know is not that some wave links here — it is the sentence it links from. So
// each row is `[source wave] [quote]`, with the linking words emphasised and an
// ellipsis at each end the kernel elided.
//
// A knowingly incomplete list says so. `truncated` and `skipped_sources` are
// rendered as one line of small text rather than dropped: a citation list that
// is quietly short is worse than one that admits it is short.

import type { WaveBacklink, WaveBacklinks } from '../../../../../core/domain/report.ts';
import { groupBacklinks } from '../../../../../core/domain/report.ts';
import styles from './backlinks.module.css';

export type ReportBacklinksProps = Readonly<{
  waveId: string;
  backlinks: WaveBacklinks;
  /** Open the citing wave, landing on the block the citation is written in. */
  onOpen: (waveId: string, blockId: string) => void;
}>;

function Quote({ backlink }: { backlink: WaveBacklink }) {
  const quote = backlink.quote;
  if (quote === null || quote === undefined) return <>{backlink.label}</>;
  return (
    <>
      {quote.head_elided && '…'}
      {quote.before}
      {quote.label !== '' && <b className={styles.hit}>{quote.label}</b>}
      {quote.after}
      {quote.tail_elided && '…'}
    </>
  );
}

export function ReportBacklinks({ waveId, backlinks, onOpen }: ReportBacklinksProps) {
  const groups = groupBacklinks(backlinks.backlinks, waveId);

  return (
    <div className={styles.backlinks}>
      <ul>
        {groups.map((group) => (
          <li key={group.waveId}>
            <ul>
              {group.entries.map((entry, index) => (
                <li key={`${entry.src_block_id}:${entry.dst_block_id ?? ''}:${index}`}>
                  {/* INV-A11Y-061: a button and a callback, like every other
                      navigation in the app. */}
                  <button
                    type="button"
                    className={styles.row}
                    onClick={() => onOpen(group.waveId, entry.src_block_id)}
                  >
                    <span className={styles.title}>{group.title}</span>
                    <span className={styles.quote}><Quote backlink={entry} /></span>
                  </button>
                </li>
              ))}
            </ul>
          </li>
        ))}
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
