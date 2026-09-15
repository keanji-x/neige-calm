// The `table` block — a comparables table, in the editorial register.
//
// No outer frame: the breakout to `--measure-doc` *is* the visual event, and
// §6.5's boundary ladder stops at "hairline per row" long before a box. The
// header is small, uppercase and tracked; numeric columns are right-aligned
// and `tabular-nums` so digits line up down the column, which is the only
// reason a table beats a list.

import type { ReactNode } from 'react';

import {
  parseReportSourceLink, type ReportSourceLinkTarget,
} from '../../../../../core/domain/report-source.ts';
import {
  inlineTableBlockPayloadSchema, isLiveTablePayload,
  type InlineTableBlockPayload, type TableBlockPayload,
} from '../../../../../core/domain/report.ts';
import { ReportSourceCitation } from '../source/public.tsx';
import styles from './table.module.css';

function cellText(value: string | number | null | undefined): string {
  return value === null || value === undefined ? '' : String(value);
}

/**
 * A cell that is, in its entirety, one Markdown inline link: `[label](dest)`
 * and nothing else once trimmed. Anchored at both ends on purpose — a link
 * with prose around it, or two links, is not this shape and stays text.
 */
const WHOLE_CELL_LINK = /^\[([^\]]*)\]\(([^\s()]+)\)$/;

/**
 * The one piece of Markdown a table cell understands (#1687): a cell whose
 * whole text is a single `[label](neige://source/…)` citation. The template
 * asks every figure to carry its source and every source to be a
 * `neige://source/…` link, so the 「来源」 column of a table is where the
 * two rules meet; the prose beside it already paints these as citations, and
 * a cell showing the raw link syntax was the one unclickable citation in
 * the report.
 *
 * Deliberately not a Markdown renderer. Cells are strings on the wire and
 * stay strings; anything else — a link with prose around it, two links, a
 * link under another scheme, markup — is text, as before. A cell that wants
 * a citation puts one link in it and nothing else, and the template says
 * so.
 */
function cellSourceCitation(text: string): { label: string; target: ReportSourceLinkTarget } | null {
  const match = WHOLE_CELL_LINK.exec(text.trim());
  if (match === null) return null;
  const target = parseReportSourceLink(match[2] ?? '');
  return target === null ? null : { label: match[1] ?? '', target };
}

function Cell({ value, onOpenSourceLink }: {
  value: string | number | null | undefined;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}): ReactNode {
  const text = cellText(value);
  const citation = cellSourceCitation(text);
  if (citation === null) return text;
  return (
    <ReportSourceCitation target={citation.target} onOpen={onOpenSourceLink}>
      {citation.label}
    </ReportSourceCitation>
  );
}

/**
 * A live table's rows come from a plugin-written overlay, so this component
 * has three states the inline form never has: the source names something that
 * has not pushed yet, the source resolved but to a payload that is not a
 * table, and no resolver was supplied at all (a surface that does not carry
 * overlays, e.g. the Today document).
 *
 * All three render the caption plus one line of prose rather than nothing: a
 * live block that silently disappears is indistinguishable from a report that
 * never had one, which is precisely the reading a reader must not be given
 * about a number they came here to check.
 */
export function ReportTableBlock({ payload, resolveLive, onOpenSourceLink }: {
  payload: TableBlockPayload;
  resolveLive?: (source: string) => unknown;
  /**
   * A cell that is one `neige://source/…` citation was activated (#1687).
   * The same handler the document gives its prose; absent ⇒ the cell is the
   * badge-plus-label form, exactly as the prose is on a surface with no
   * panel.
   */
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  if (isLiveTablePayload(payload)) {
    if (resolveLive === undefined) {
      return <LiveTableNotice caption={payload.caption}
        text="This table is live and this view does not carry live data." />;
    }
    const resolved = resolveLive(payload.source);
    if (resolved === undefined) {
      return <LiveTableNotice caption={payload.caption}
        text={`Waiting for ${payload.source} — nothing has been pushed here yet.`} />;
    }
    const decoded = inlineTableBlockPayloadSchema.safeParse(resolved);
    if (!decoded.success) {
      return <LiveTableNotice caption={payload.caption}
        text={`${payload.source} holds something this build cannot read as a table.`} />;
    }
    // The live payload's own caption wins when it has one: it is written by
    // whoever produced these exact rows (a timestamp, a "priced in USDT"), and
    // the block's caption is the document author's standing description.
    return <InlineTable payload={decoded.data} fallbackCaption={payload.caption} onOpenSourceLink={onOpenSourceLink} />;
  }
  return <InlineTable payload={payload} onOpenSourceLink={onOpenSourceLink} />;
}

function LiveTableNotice({ caption, text }: { caption?: string | null; text: string }) {
  return (
    <div className={styles.wrap}>
      {caption != null && caption !== '' && <p className={styles.caption}>{caption}</p>}
      <p className={styles.caption}>{text}</p>
    </div>
  );
}

function InlineTable({ payload, fallbackCaption, onOpenSourceLink }: {
  payload: InlineTableBlockPayload;
  fallbackCaption?: string | null;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  const { columns, rows, highlight } = payload;
  const caption = payload.caption ?? fallbackCaption;
  // A row is addressed by its first column's value — the natural identity of a
  // row in a comparables table, and the only key the payload offers.
  const keyColumn = columns[0]?.key;

  return (
    // Its own scroll container: a wide table may not make the page scroll
    // sideways (§3.2).
    <div className={styles.wrap}>
      <table className={styles.table}>
        {caption != null && caption !== '' && <caption className={styles.caption}>{caption}</caption>}
        <thead>
          <tr>
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                className={column.align === 'right' ? `${styles.head} ${styles.right}` : styles.head}
              >
                {column.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => {
            const highlighted = highlight != null && highlight !== ''
              && keyColumn !== undefined && cellText(row[keyColumn]) === highlight;
            return (
              <tr key={index} className={highlighted ? styles.highlighted : undefined}>
                {columns.map((column) => (
                  <td
                    key={column.key}
                    className={column.align === 'right' ? `${styles.cell} ${styles.right}` : styles.cell}
                  >
                    <Cell value={row[column.key]} onOpenSourceLink={onOpenSourceLink} />
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
