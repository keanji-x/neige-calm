// The `chart.series` block — S1 placeholder (#1628).
//
// A `chart.series` block names its data (a plugin tool + asset ids) instead
// of carrying it, and the kernel resolves the points on the read path. That
// read path does not exist yet, so this slice draws no figure: it renders
// the caption and one line stating what the block asks for, in the same
// register a live table uses while it waits for its source. The point is
// that a report which already contains such a block reads as a document
// with a chart pending, not as one with a hole or an "unsupported" line.
//
// No requests, no chart library, no literal colours: the line is text, and
// the text is themed by the same tokens as everything else. A later slice
// replaces this component wholesale.

import type { ChartSeriesPayload } from '../../../../../core/domain/report.ts';
import styles from './series.module.css';

export function ReportSeriesBlock({ payload }: { payload: ChartSeriesPayload }) {
  const { caption } = payload;
  const range = payload.range ?? '1Y';
  const period = payload.period ?? 'day';
  const view = payload.view ?? 'line';
  return (
    <div className={styles.wrap}>
      {caption != null && caption !== '' && <p className={styles.caption}>{caption}</p>}
      <p className={styles.note} role="note">
        {`chart.series · ${payload.series.join(', ')} · ${range} ${period} ${view}`}
        {' — chart rendering lands in a later slice'}
      </p>
    </div>
  );
}
