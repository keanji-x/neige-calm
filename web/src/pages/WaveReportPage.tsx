import { lazy, Suspense, useEffect, useMemo } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../api/calm';
import { useWaveFileContent } from '../api/queries';
import { useTheme } from '../app/theme';
import type { Wave, WaveCardSlot } from '../types';
import { waveDisplayTitle } from '../shared/waveTitle';
import { useState } from '../shared/state';
import type { WaveReportCardData } from '../cards/builtins/wave-report';
import { WaveFileTree } from '../cards/wave-file-tree';
import { SpecCurrentRun } from './SpecCurrentRun';

export interface WaveReportPageProps {
  wave: Wave;
  cards: WaveCardSlot[];
}

type CardSlot = Extract<WaveCardSlot, { kind: 'card' }>;
type ReportCardSlot = CardSlot & { card: WaveReportCardData };

const LazyCodePane = lazy(() =>
  import('../cards/builtins/file-viewer-codemirror').then((m) => ({
    default: m.CodePane,
  })),
);

function isReportSlot(slot: WaveCardSlot): slot is ReportCardSlot {
  return slot.kind === 'card' && slot.card.type === 'wave-report';
}

function selectReportCards(cards: WaveCardSlot[]): ReportCardSlot[] {
  const reports = cards.filter(isReportSlot);
  return reports.slice().sort((a, b) => (a.sort ?? 0) - (b.sort ?? 0));
}

function selectSpecCard(cards: WaveCardSlot[]): string | null {
  const slot = cards.find(
    (s): s is CardSlot => s.kind === 'card' && s.card.type === 'spec',
  );
  return slot?.card.id ?? null;
}

function formatUpdatedAt(updatedAt?: number): string {
  if (
    typeof updatedAt !== 'number' ||
    !Number.isFinite(updatedAt) ||
    updatedAt <= 0
  ) {
    return 'Updated -';
  }

  const diffMs = Math.max(0, Date.now() - updatedAt);
  const minutes = Math.floor(diffMs / 60_000);
  if (minutes < 1) return 'Updated just now';
  if (minutes < 60) return `Updated ${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Updated ${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 30) return `Updated ${days}d ago`;

  return `Updated ${new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
  }).format(new Date(updatedAt))}`;
}

function readTime(body: string | undefined): string {
  const words = (body ?? '').trim().split(/\s+/).filter(Boolean).length;
  if (words === 0) return 'Read time -';
  return `${Math.max(1, Math.ceil(words / 220))} min read`;
}

function ReportByline({ report }: { report?: WaveReportCardData }) {
  return (
    <div className="report-byline" aria-label="Report metadata">
      <span className="report-byline-author">
        <span className="report-byline-avatar" aria-hidden="true">
          R
        </span>
        <span>Research Agent</span>
      </span>
      <span className="report-byline-sep" aria-hidden="true" />
      <span>{formatUpdatedAt(report?.updatedAt)}</span>
      <span className="report-byline-sep" aria-hidden="true" />
      <span>Sources -</span>
      <span className="report-byline-sep" aria-hidden="true" />
      <span>{readTime(report?.body)}</span>
    </div>
  );
}

function DuplicateReportBanner({ count }: { count: number }) {
  return (
    <div className="report-duplicate" role="status" data-count={count}>
      Multiple report cards found. Showing the earliest.
    </div>
  );
}

function ReportEmptyState() {
  return (
    <div className="report-empty" role="status">
      Report not ready. The spec agent has not produced a report yet.
    </div>
  );
}

function ReportContent({
  waveId,
  path,
  reportCardBody,
}: {
  waveId: string;
  path: string;
  reportCardBody?: string;
}) {
  const contentQ = useWaveFileContent(waveId, path, { enabled: true });
  const isReportMissing =
    path === 'report.md' &&
    contentQ.error instanceof CalmApiError &&
    contentQ.error.status === 404;
  const isReportUnavailable =
    isReportMissing ||
    (path === 'report.md' && isRelativeFetchUrlError(contentQ.error));
  const isFetching = queryIsFetching(contentQ);
  const shouldFallbackToReportCard =
    path === 'report.md' &&
    !!reportCardBody &&
    (isReportUnavailable ||
      (!contentQ.data && !contentQ.error) ||
      (contentQ.isLoading && isFetching));

  if (contentQ.isLoading) {
    if (path === 'report.md' && isFetching) {
      return shouldFallbackToReportCard ? (
        <ReportMarkdown body={reportCardBody ?? ''} />
      ) : (
        <ReportEmptyState />
      );
    }
    return (
      <div className="report-empty" role="status">
        Loading…
      </div>
    );
  }

  if (shouldFallbackToReportCard) {
    return <ReportMarkdown body={reportCardBody ?? ''} />;
  }

  if (isReportUnavailable) {
    return <ReportEmptyState />;
  }

  if (contentQ.error) {
    return <InlineApiError error={contentQ.error} />;
  }

  if (!contentQ.data) {
    return <ReportEmptyState />;
  }

  if (contentQ.data.content_type === 'text/markdown') {
    return <ReportMarkdown body={contentQ.data.content} />;
  }

  if (isTextContent(contentQ.data.content_type)) {
    return (
      <div className="report-code">
        <ReportCodeContent path={path} text={contentQ.data.content} />
      </div>
    );
  }

  return (
    <div className="report-empty" role="status">
      Preview unavailable for {contentQ.data.content_type}
    </div>
  );
}

function ReportMarkdown({ body }: { body: string }) {
  return (
    <div className="report-prose">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{body}</ReactMarkdown>
    </div>
  );
}

function ReportCodeContent({ path, text }: { path: string; text: string }) {
  const { resolved: theme } = useTheme();

  return (
    <Suspense
      fallback={
        <div className="report-empty" role="status">
          Loading viewer…
        </div>
      }
    >
      <LazyCodePane path={path} text={text} theme={theme} />
    </Suspense>
  );
}

function InlineApiError({ error }: { error: Error }) {
  return (
    <div role="alert" className="report-empty report-error">
      {formatApiError(error)}
    </div>
  );
}

function formatApiError(error: Error): string {
  if (error instanceof CalmApiError) {
    return error.message || error.code || `HTTP ${error.status}`;
  }
  return error.message || 'Request failed';
}

function isTextContent(contentType: string): boolean {
  return (
    contentType.startsWith('text/') ||
    contentType === 'application/json' ||
    contentType.endsWith('+json') ||
    contentType === 'application/xml' ||
    contentType.endsWith('+xml')
  );
}

function isRelativeFetchUrlError(error: Error | null): boolean {
  return (
    error instanceof TypeError &&
    error.message.startsWith('Failed to parse URL from /api/waves/')
  );
}

function queryIsFetching(query: unknown): boolean {
  if (typeof query !== 'object' || query === null || !('fetchStatus' in query)) {
    return false;
  }
  return (query as { fetchStatus?: unknown }).fetchStatus === 'fetching';
}

export function WaveReportPage({ wave, cards }: WaveReportPageProps) {
  const title = waveDisplayTitle(wave.title);
  const reportSlots = selectReportCards(cards);
  const reportCard = reportSlots[0]?.card;
  const specCardId = useMemo(() => selectSpecCard(cards), [cards]);
  const [selectedFilePath, setSelectedFilePath] = useState<string>('report.md');

  useEffect(() => {
    setSelectedFilePath('report.md');
  }, [wave.id]);

  return (
    <div className="report-page">
      <section className="report-center" aria-label="Report">
        <article className="report-doc">
          {reportSlots.length > 1 && (
            <DuplicateReportBanner count={reportSlots.length} />
          )}
          <h1 className="report-title">{title}</h1>
          <ReportByline report={reportCard} />
          <ReportContent
            waveId={wave.id}
            path={selectedFilePath}
            reportCardBody={reportCard?.body}
          />
        </article>
      </section>
      <aside className="report-rail" aria-label="Report context">
        <section className="report-rail-section" aria-label="Files">
          <header className="report-rail-head">
            <span>Files</span>
          </header>
          <div className="report-rail-files">
            <WaveFileTree
              waveId={wave.id}
              selectedPath={selectedFilePath}
              onSelectedPathChange={(path) =>
                setSelectedFilePath(path ?? 'report.md')
              }
              ariaLabel="Wave files"
              fallback={<div className="report-rail-placeholder">No files yet.</div>}
            />
          </div>
        </section>
        <section className="report-rail-section" aria-label="Event line">
          <header className="report-rail-head">
            <span>Event line</span>
          </header>
          <div className="report-rail-placeholder">
            Activity timeline appears here. (Wired in PR-E.)
          </div>
        </section>
      </aside>
      <SpecCurrentRun specCardId={specCardId} />
    </div>
  );
}
