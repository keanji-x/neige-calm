import { lazy, Suspense, useMemo } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../api/calm';
import { useWaveFileContent } from '../api/queries';
import { useTheme } from '../app/theme';
import type { Wave, WaveCardSlot } from '../types';
import { waveDisplayTitle } from '../shared/waveTitle';
import { useState } from '../shared/state';
import { formatUpdatedAt } from '../shared/relativeTime';
import type { WaveReportCardData } from '../cards/builtins/wave-report';
import { WaveFileTree } from '../cards/wave-file-tree';
import { useWaveFsViewer } from '../wave-fs-viewers';
import { EventLinePanel } from './EventLinePanel';
import { SpecConversation, type ReportView } from './SpecConversation';
import { ChevronIcon } from '../shared/components/ChevronIcon';
import { useAnyRuntimeLive, useEventLineEntries } from './useEventLineEntries';

export interface WaveReportPageProps {
  wave: Wave;
  cards: WaveCardSlot[];
}

const REPORT_RAIL_COLLAPSED_STORAGE_KEY = 'calm:report-rail:collapsed';

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

function readReportRailCollapsed(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return window.localStorage.getItem(REPORT_RAIL_COLLAPSED_STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}

function writeReportRailCollapsed(collapsed: boolean): void {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(
      REPORT_RAIL_COLLAPSED_STORAGE_KEY,
      collapsed ? 'true' : 'false',
    );
  } catch {
    // localStorage may throw in private browsing or under quota pressure.
  }
}

function ReportByline({ report }: { report?: WaveReportCardData }) {
  return (
    <div className="report-byline" aria-label="Report metadata">
      <span className="report-byline-author">
        <span className="report-byline-avatar" aria-hidden="true">
          S
        </span>
        <span>Spec Agent</span>
      </span>
      <span className="report-byline-sep" aria-hidden="true" />
      <span>{formatUpdatedAt(report?.updatedAt)}</span>
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
    (!!contentQ.error ||
      (!contentQ.data && !contentQ.error) ||
      (contentQ.isLoading && isFetching));
  const jsonViewer = useWaveFsViewer(
    contentQ.data && isJsonContent(contentQ.data.content_type) ? path : '',
    contentQ.data && isJsonContent(contentQ.data.content_type)
      ? contentQ.data.content
      : '',
  );

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

  if (isJsonContent(contentQ.data.content_type) && jsonViewer) {
    const { Viewer, data, raw } = jsonViewer;
    return (
      <div className="report-json-card">
        <Viewer data={data} path={path} raw={raw} />
      </div>
    );
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
    <div className="report-prose calm-prose">
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
    isJsonContent(contentType) ||
    contentType === 'application/xml' ||
    contentType.endsWith('+xml')
  );
}

function isJsonContent(contentType: string): boolean {
  return contentType === 'application/json' || contentType.endsWith('+json');
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
  const hasReportCard = reportSlots.length > 0;
  const reportCard = reportSlots[0]?.card;
  const specCardId = useMemo(() => selectSpecCard(cards), [cards]);
  const [selectedFilePath, setSelectedFilePath] = useState<string>('report.md');
  const [view, setView] = useState<ReportView>('report');
  const [lastWaveId, setLastWaveId] = useState<string>(wave.id);
  const [lastSpecCardId, setLastSpecCardId] = useState<string | null>(
    specCardId,
  );
  const [reportRailCollapsed, setReportRailCollapsed] = useState(
    () => readReportRailCollapsed(),
  );
  const [showHiddenFiles, setShowHiddenFiles] = useState(false);
  const eventEntries = useEventLineEntries(wave.id, cards);
  const live = useAnyRuntimeLive(wave.id, cards);

  // Sync reset during render so a new wave never renders with the old file path.
  if (lastWaveId !== wave.id) {
    setLastWaveId(wave.id);
    setSelectedFilePath('report.md');
    setView('report');
  }

  // When the spec card disappears, drop the stale conversation view so a
  // later card reappearance does not snap back to conversation (and steal
  // focus into its input).
  if (lastSpecCardId !== specCardId) {
    setLastSpecCardId(specCardId);
    if (specCardId == null) setView('report');
  }

  const toggleReportRailCollapsed = () => {
    setReportRailCollapsed((current) => {
      const next = !current;
      writeReportRailCollapsed(next);
      return next;
    });
  };
  const toggleHiddenFiles = () => {
    setShowHiddenFiles((current) => !current);
  };

  const railCollapseButton = (
    <button
      type="button"
      className="report-rail-toggle"
      onClick={toggleReportRailCollapsed}
      aria-expanded={!reportRailCollapsed}
      aria-label={
        reportRailCollapsed ? 'Expand report rail' : 'Collapse report rail'
      }
      title={reportRailCollapsed ? 'Expand report rail' : 'Collapse report rail'}
    >
      <ChevronIcon />
    </button>
  );

  return (
    <div
      className={
        'report-page' + (reportRailCollapsed ? ' report-page--rail-collapsed' : '')
      }
    >
      <section className="report-center" aria-label="Report">
        <SpecConversation
          specCardId={specCardId}
          view={specCardId == null ? 'report' : view}
          onViewChange={setView}
        >
          <article className="report-doc">
            {reportSlots.length > 1 && (
              <DuplicateReportBanner count={reportSlots.length} />
            )}
            <h1 className="report-title">{title}</h1>
            <ReportByline report={reportCard} />
            {hasReportCard || selectedFilePath !== 'report.md' ? (
              <ReportContent
                waveId={wave.id}
                path={selectedFilePath}
                reportCardBody={reportCard?.body}
              />
            ) : (
              <ReportEmptyState />
            )}
          </article>
        </SpecConversation>
      </section>
      <aside
        className={
          'report-rail' + (reportRailCollapsed ? ' report-rail--collapsed' : '')
        }
        aria-label="Report context"
      >
        <header className="report-rail-head report-rail-head--top">
          {!reportRailCollapsed && <span>Files</span>}
          <div className="report-rail-actions">
            {!reportRailCollapsed && (
              <button
                type="button"
                className="report-rail-toggle report-rail-toggle--show-all"
                aria-pressed={showHiddenFiles}
                onClick={toggleHiddenFiles}
              >
                Show all
              </button>
            )}
            {railCollapseButton}
          </div>
        </header>
        {!reportRailCollapsed && (
          <>
            <section className="report-rail-section" aria-label="Files">
              <div className="report-rail-files">
                <WaveFileTree
                  waveId={wave.id}
                  selectedPath={selectedFilePath}
                  onSelectedPathChange={(path) => {
                    setSelectedFilePath(path ?? 'report.md');
                    // Selecting a file always shows the document view.
                    setView('report');
                  }}
                  ariaLabel="Wave files"
                  showHidden={showHiddenFiles}
                  fallback={
                    <div className="report-rail-placeholder">No files yet.</div>
                  }
                />
              </div>
            </section>
            <section className="report-rail-section" aria-label="Event line">
              <EventLinePanel entries={eventEntries} live={live} />
            </section>
          </>
        )}
      </aside>
    </div>
  );
}
