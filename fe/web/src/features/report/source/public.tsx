// The source panel: what a `neige://source/…` citation opens.
// The body is raw text, never Markdown: a source is evidence, and rendering it would let an untrusted document draw links and images.

import { useEffect, useRef, type ReactNode } from 'react';

import type {
  ReportSourceLinkTarget, SourceResolution, TrackSourceDetail,
} from '../../../../../core/domain/report-source.ts';
import { sourceHighlight } from '../../../../../core/domain/report-source.ts';
import { ErrorBox } from '../../../ui/error-box/public.tsx';
import { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';
import styles from './source.module.css';

export { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';

/** The citation, as the document's prose and the table's cells both paint it. With a handler it is a `<button>` even when the id or anchor will not parse; without one it is the badge and its label. */
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

/** The drawer's accessible name for this state: the row's title once known, the generic word until then. */
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

/** The missing state, in one shape for its three causes: dangling id, malformed link, or an anchor the row does not carry. */
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
      {anchorMissed && (
        <MissingNotice
          destination={target.destination}
          reason="anchor"
          title={SOURCE_PANEL_COPY.anchorMissingTitle}
          detail={SOURCE_PANEL_COPY.anchorMissingDetail}
        />
      )}
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
          {/* Text, not `<a href>`: the URL is planner-declared, so it steers nothing. */}
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
