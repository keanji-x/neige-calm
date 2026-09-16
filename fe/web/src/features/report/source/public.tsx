// The source panel (#1669 §2.5): what a `neige://source/…` citation opens.
//
// A citation used to be a title, a date and a content id the reader could do
// nothing with. The kernel now keeps the text the planner read — the plugin
// call's own reply, byte for byte — and this panel shows it: the provenance
// badge first (whether this is the institution's text, 智堡's summary, a web
// page, or the planner's own hand), then the title and the row's facts, then
// the body **as raw text**. Not Markdown: a source is evidence, and rendering
// it would let an untrusted document draw links and images in the reader's
// own product. Monospace, wrapped, nothing interpreted.
//
// With an anchor, the quote is sliced out of the body by `indexOf` (first
// occurrence — `core/domain/report-source`), painted as a `<mark>` and
// scrolled into view. A quote that cannot be placed leaves the body whole and
// says so above it; a citation the track cannot answer says "来源缺失" and
// shows the destination as written.
//
// It fetches nothing. `resolution` is the app's query, handed in the way the
// series block receives its resolver: `features/**` may not import `app/**`,
// and the query — its key, its 404-is-a-state rule — is the app's.

import { useEffect, useRef, type ReactNode } from 'react';

import type {
  ReportSourceLinkTarget, SourceResolution, TrackSourceDetail,
} from '../../../../../core/domain/report-source.ts';
import { sourceHighlight } from '../../../../../core/domain/report-source.ts';
import { ErrorBox } from '../../../ui/error-box/public.tsx';
import { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';
import styles from './source.module.css';

/* The feature's one public entry: the document's inline citation badge and
   the app's drawer chrome read their words through here, so `copy.ts` stays
   internal to this directory. */
export { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';

/**
 * The citation itself, as the document's prose and the table's cells both
 * paint it (#1669 §2.5, #1687). One component, because the two surfaces
 * used to disagree: the table showed the raw `[label](neige://source/…)`
 * while the prose beside it was clickable.
 *
 * With a handler it is a control — a `<button>`, never an `<a href>`
 * (INV-A11Y-061) — and that includes a citation whose id or anchor will not
 * parse: the panel is where "来源缺失" is said, so a malformed citation must
 * still be reachable. Without a handler — on Today or a Markdown file — it
 * is the 「来源」 badge and its label, without an inactive button. The badge
 * still says what the
 * label is. The badge carries no provenance because nothing on this path has
 * read the row.
 */
export function ReportSourceCitation({ target, onOpen, children }: {
  target: ReportSourceLinkTarget;
  onOpen?: (target: ReportSourceLinkTarget) => void;
  /** The link's label, already rendered. */
  children: ReactNode;
}) {
  if (onOpen !== undefined) {
    return (
      <button
        type="button"
        className={styles.citationLink}
        data-nc-report-source-link=""
        onClick={() => onOpen(target)}
      >
        {children}
      </button>
    );
  }
  return (
    <span className={styles.citation} data-nc-report-source-citation="">
      <span className={styles.citationBadge}>{SOURCE_PANEL_COPY.citationBadge}</span>
      {children}
    </span>
  );
}

export type ReportSourcePanelProps = Readonly<{
  /** The citation that opened the panel. */
  target: ReportSourceLinkTarget;
  /** The app's read of `target.sourceId`; ignored when the link never named one. */
  resolution: SourceResolution;
  /** Repeats the read after a transport failure — a read, never a write. */
  onRetry: () => void;
}>;

/**
 * The drawer's accessible name for this state: the row's title once it is
 * known, the generic word until then. Exported so the app can name the
 * `<Drawer>` it wraps this in without re-deriving the rule.
 */
export function reportSourcePanelTitle(resolution: SourceResolution): string {
  return resolution.status === 'ok' ? resolution.source.title : SOURCE_PANEL_COPY.panelTitle;
}

export function ReportSourcePanel({ target, resolution, onRetry }: ReportSourcePanelProps) {
  return (
    <div className={styles.panel} data-nc-report-source="">
      {target.sourceId === null
        ? <Missing target={target} reason="malformed" />
        : resolution.status === 'missing'
          ? <Missing target={target} reason="dangling" />
          : resolution.status === 'loading'
            ? <p className={styles.state} role="status">{SOURCE_PANEL_COPY.loading}</p>
            : resolution.status === 'error'
              ? <ErrorBox message="无法读取来源。" details={resolution.message} onRetry={onRetry} />
              : <Source source={resolution.source} target={target} />}
    </div>
  );
}

/**
 * The missing state, in one shape for its three causes (§2.5: the panel says
 * the citation is missing and prints the destination as written, because
 * that is the one thing the reader can act on). A dangling id is the
 * design's own admitted state (a recipe-born track, a link written for
 * another track); a link that will not parse is the author's typo; an
 * anchor the row does not carry is a citation written ahead of — or against
 * — the source's quotes. The first two are the whole panel; the third sits
 * above a source that is still shown, so its heading is one rank down.
 */
function MissingNotice({ destination, reason, title, detail }: {
  destination: string;
  reason: 'dangling' | 'malformed' | 'anchor';
  title: string;
  detail: string;
}) {
  const Heading = reason === 'anchor' ? 'h3' : 'h2';
  return (
    <section className={styles.missing} data-nc-report-source-missing={reason} aria-live="polite">
      <Heading className={styles.missingTitle}>{title}</Heading>
      <p className={styles.state}>{detail}</p>
      <p className={styles.destination}>
        <span className={styles.metaLabel}>{SOURCE_PANEL_COPY.destinationLabel}</span>
        <code className={styles.destinationCode}>{destination}</code>
      </p>
    </section>
  );
}

function Missing({ target, reason }: { target: ReportSourceLinkTarget; reason: 'dangling' | 'malformed' }) {
  return (
    <MissingNotice
      destination={target.destination}
      reason={reason}
      title={SOURCE_PANEL_COPY.missingTitle}
      detail={reason === 'dangling' ? SOURCE_PANEL_COPY.missingDangling : SOURCE_PANEL_COPY.missingMalformed}
    />
  );
}

function Source({ source, target }: { source: TrackSourceDetail; target: ReportSourceLinkTarget }) {
  const { quoteId } = target;
  const highlight = quoteId === null ? null : sourceHighlight(source, quoteId);
  const anchorMissed = quoteId !== null && highlight === null;
  const markRef = useRef<HTMLElement | null>(null);

  /*
   * Scroll to the quote once it is painted. `block: 'center'` rather than
   * `start`: the reader wants the sentence in context, and a quote pinned to
   * the top edge of the pane reads as the beginning of something rather than
   * the middle of it. Re-run on the quote, not on every render — the body is
   * immutable, so the mark only moves when the citation does.
   */
  const placed = highlight !== null;
  useEffect(() => {
    if (!placed) return;
    markRef.current?.scrollIntoView({ block: 'center' });
  }, [placed, quoteId, source.source_id]);

  return (
    <article className={styles.source}>
      <header className={styles.head}>
        <span className={styles.badge} data-nc-report-source-provenance={source.provenance}>
          {SOURCE_PROVENANCE_COPY[source.provenance]}
        </span>
        <h2 className={styles.title}>{source.title}</h2>
        <dl className={styles.meta}>
          {source.published_at !== undefined && source.published_at !== '' && (
            <div className={styles.metaRow}>
              <dt className={styles.metaLabel}>{SOURCE_PANEL_COPY.publishedAt}</dt>
              <dd className={styles.metaValue}>{source.published_at}</dd>
            </div>
          )}
          <div className={styles.metaRow}>
            <dt className={styles.metaLabel}>{SOURCE_PANEL_COPY.capturedAt}</dt>
            <dd className={styles.metaValue}><time dateTime={source.captured_at}>{capturedAtLabel(source.captured_at)}</time></dd>
          </div>
          <Origin source={source} />
        </dl>
      </header>
      {/* The source exists, the anchor does not: the missing state, with the
          destination as written, above a body that is still worth reading. */}
      {anchorMissed && (
        <MissingNotice
          destination={target.destination}
          reason="anchor"
          title={SOURCE_PANEL_COPY.anchorMissingTitle}
          detail={SOURCE_PANEL_COPY.anchorMissingDetail}
        />
      )}
      {/* `<pre>`, never a Markdown pass: see the file header. */}
      <pre className={styles.body} data-nc-report-source-body="">
        {highlight === null
          ? source.body
          : (
            <>
              {highlight.before}
              <mark ref={markRef} className={styles.quote} data-nc-report-source-quote={quoteId ?? ''}>
                {highlight.quote}
              </mark>
              {highlight.after}
            </>
          )}
      </pre>
    </article>
  );
}

/**
 * Where the body came from, in the kernel's own terms: the plugin and tool
 * whose reply it is, or, for a manual row, whatever the planner declared.
 * The content id / URL rides on the row itself and prints for either kind.
 */
function Origin({ source }: { source: TrackSourceDetail }) {
  const { origin } = source;
  return (
    <>
      {origin.kind === 'plugin' && (
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>{SOURCE_PANEL_COPY.origin}</dt>
          <dd className={`${styles.metaValue} ${styles.mono}`}>
            {SOURCE_PANEL_COPY.originPlugin} {origin.plugin_id} · {SOURCE_PANEL_COPY.originTool} {origin.tool}
          </dd>
        </div>
      )}
      {source.content_id !== undefined && source.content_id !== '' && (
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>{SOURCE_PANEL_COPY.contentId}</dt>
          <dd className={`${styles.metaValue} ${styles.mono}`}>{source.content_id}</dd>
        </div>
      )}
      {source.url !== undefined && source.url !== '' && (
        <div className={styles.metaRow}>
          <dt className={styles.metaLabel}>{SOURCE_PANEL_COPY.url}</dt>
          {/* Text, not `<a href>`: INV-A11Y-061 holds on this surface too, and
              the URL is planner-declared, so it steers nothing. */}
          <dd className={`${styles.metaValue} ${styles.mono}`}>{source.url}</dd>
        </div>
      )}
    </>
  );
}

/** The capture instant in the reader's locale; the raw RFC 3339 stays on `dateTime`. */
function capturedAtLabel(capturedAt: string): string {
  const instant = new Date(capturedAt);
  return Number.isNaN(instant.getTime()) ? capturedAt : instant.toLocaleString();
}
