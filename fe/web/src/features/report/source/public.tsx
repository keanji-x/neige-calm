// The source panel: what a `neige://source/…` citation opens.
// Captured evidence has an inert reading view and an exact original-text view.

import { useEffect, useRef } from 'react';

import type {
  ReportSourceLinkTarget, SourceResolution, TrackSourceDetail,
} from '../../../../../core/domain/report-source.ts';
import { sourceHighlight } from '../../../../../core/domain/report-source.ts';
import { readFailureText } from '../../../../../core/domain/read-failure.ts';
import { useState } from '../../../ui/state/public.ts';
import { ProseBlock } from '../document/content.tsx';
import { ErrorBox } from '../../../ui/error-box/public.tsx';
import { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';
import styles from './source.module.css';

export { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';

export { ReportSourceCitation } from './citation.tsx';

export type ReportSourcePanelProps = Readonly<{
  /** The citation that opened the panel. */
  target: ReportSourceLinkTarget;
  /** The app's read of `target.sourceId`; ignored when the link never named one. */
  resolution: SourceResolution;
  /** Repeats the read after a transport failure — a read, never a write. */
  onRetry: () => void;
  /** Hover previews must not scroll the underlying report. */
  scrollToQuote?: boolean;
}>;

/** The drawer's accessible name for this state: the row's title once known, the generic word until then. */
export function reportSourcePanelTitle(resolution: SourceResolution): string {
  return resolution.status === 'ok' ? resolution.source.title : SOURCE_PANEL_COPY.panelTitle;
}

export function ReportSourcePanel({ target, resolution, onRetry, scrollToQuote = true }: ReportSourcePanelProps) {
  return (
    <div className={styles.panel} data-nc-report-source="">
      {target.sourceId === null
        ? <Missing target={target} reason="malformed" />
        : resolution.status === 'missing'
          ? <Missing target={target} reason="dangling" />
          : resolution.status === 'loading'
            ? <p className={styles.state} role="status">{SOURCE_PANEL_COPY.loading}</p>
            : resolution.status === 'error'
              ? <ErrorBox message={readFailureText(resolution.failure, '无法读取来源。')} onRetry={onRetry} />
              : <Source key={`${resolution.source.source_id}:${target.quoteId ?? ''}`} source={resolution.source} target={target} scrollToQuote={scrollToQuote} />}
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

function Source({ source, target, scrollToQuote }: { source: TrackSourceDetail; target: ReportSourceLinkTarget; scrollToQuote: boolean }) {
  const { quoteId } = target;
  const [original, setOriginal] = useState(quoteId !== null);
  const highlight = quoteId === null ? null : sourceHighlight(source, quoteId);
  const anchorMissed = quoteId !== null && highlight === null;
  const markRef = useRef<HTMLElement | null>(null);

  const placed = highlight !== null;
  useEffect(() => {
    if (!placed || !original || !scrollToQuote) return;
    markRef.current?.scrollIntoView({ block: 'center' });
  }, [placed, original, quoteId, source.source_id, scrollToQuote]);

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
      <div className={styles.views} role="group" aria-label={SOURCE_PANEL_COPY.viewLabel}>
        <button type="button" className={styles.view} aria-pressed={!original} onClick={() => { setOriginal(false); }}>{SOURCE_PANEL_COPY.reading}</button>
        <button type="button" className={styles.view} aria-pressed={original} onClick={() => { setOriginal(true); }}>{SOURCE_PANEL_COPY.original}</button>
      </div>
      {original ? <pre className={styles.body} data-nc-report-source-body="">
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
      </pre> : <div className={styles.reading} data-nc-report-source-reading="">
        <ProseBlock markdown={source.body} blockId={null} destinationMode="inert" />
      </div>}
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
