import { lazy, Suspense, useEffect, useMemo, type ReactNode } from 'react';
import { Link, useRouterState } from '@tanstack/react-router';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../api/calm';
import {
  useWaveBacklinksQuery,
  useWaveFileContent,
  useWavesByCoveQuery,
} from '../api/queries';
import type { WaveBacklink, WaveBacklinksResponse } from '../api/calm';
import { useTheme } from '../app/theme';
import type { Wave, WaveCardSlot } from '../types';
import { waveDisplayTitle } from '../shared/waveTitle';
import { useState } from '../shared/state';
import { formatUpdatedAt } from '../shared/relativeTime';
import type {
  ReportBlock,
  WaveReportCardData,
} from '../cards/builtins/wave-report';
import { WaveFileTree } from '../cards/wave-file-tree';
import {
  ReportBlockView,
  ReportLink,
  reportUrlTransform,
} from './report-blocks';
import { useWaveFsViewer } from '../wave-fs-viewers';
import { SpecConversation, type ReportView } from './SpecConversation';
import { ChevronIcon } from '../shared/components/ChevronIcon';
import { deriveOutline, type ReportOutlineItem } from './report-outline';
import { deriveInterimReportOutlinks } from './report-outlinks-interim';

function decodeHash(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

export interface WaveReportPageProps {
  wave: Wave;
  cards: WaveCardSlot[];
}

const REPORT_RAIL_COLLAPSED_STORAGE_KEY = 'calm:report-rail:collapsed';
const REPORT_OUTLINKS_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:outlinks:collapsed';
const REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:backlinks:collapsed';
const REPORT_FILES_COLLAPSED_STORAGE_KEY = 'calm:report-rail:files:collapsed';

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
    const stored = window.localStorage.getItem(
      REPORT_RAIL_COLLAPSED_STORAGE_KEY,
    );
    if (stored != null) return stored === 'true';
  } catch {
    // Fall through to the responsive default when storage is unavailable.
  }
  return window.matchMedia?.('(max-width: 980px)').matches ?? false;
}

function readRailSectionCollapsed(key: string): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return window.localStorage.getItem(key) === 'true';
  } catch {
    return false;
  }
}

function writeCollapsedState(key: string, collapsed: boolean): void {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(key, collapsed ? 'true' : 'false');
  } catch {
    // localStorage may throw in private browsing or under quota pressure.
  }
}

function revealReportBlock(blockId: string): void {
  const block = document.getElementById(blockId);
  if (!block?.classList.contains('report-block')) return;
  block.scrollIntoView();
  block.classList.remove('report-block--highlight');
  requestAnimationFrame(() => block.classList.add('report-block--highlight'));
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

function UnsupportedReportVersionState() {
  return (
    <div className="report-empty report-error" role="alert">
      版本不支持，请刷新
    </div>
  );
}

function ReportContent({
  waveId,
  path,
  reportCardBody,
  reportCardBlocks,
}: {
  waveId: string;
  path: string;
  reportCardBody?: string;
  reportCardBlocks?: ReportBlock[];
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
        <ReportMarkdown body={reportCardBody ?? ''} blocks={reportCardBlocks} />
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
    return <ReportMarkdown body={reportCardBody ?? ''} blocks={reportCardBlocks} />;
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
    return (
      <ReportMarkdown
        body={contentQ.data.content}
        blocks={path === 'report.md' ? reportCardBlocks : undefined}
      />
    );
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

export function ReportMarkdown({
  body,
  blocks,
}: {
  body: string;
  blocks?: ReportBlock[];
}) {
  const hash = useRouterState({
    select: (state) => state.location.hash,
  });
  const renderedBlockIds = blocks?.map((block) => block.id).join('\0') ?? '';

  useEffect(() => {
    const blockId = decodeHash(hash);
    if (!blockId) return;
    revealReportBlock(blockId);
  }, [hash, renderedBlockIds]);

  if (blocks) {
    return (
      <>
        {blocks.map((block) => (
          <ReportBlockView key={block.id} block={block} />
        ))}
      </>
    );
  }
  return (
    <div className="report-block report-prose calm-prose">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        urlTransform={reportUrlTransform}
        components={{ a: ReportLink }}
      >
        {body}
      </ReactMarkdown>
    </div>
  );
}

function RailSection({
  title,
  count,
  collapsed,
  onCollapsedChange,
  actions,
  children,
}: {
  title: string;
  count?: number;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="report-rail-section" aria-label={title}>
      <header className="report-rail-head">
        <h2>{title}</h2>
        {count != null && <span className="report-rail-count">{count}</span>}
        <div className="report-rail-actions">
          {actions}
          <button
            type="button"
            className="report-section-toggle"
            aria-expanded={!collapsed}
            aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${title}`}
            onClick={() => onCollapsedChange(!collapsed)}
          >
            <ChevronIcon />
          </button>
        </div>
      </header>
      {!collapsed && <div className="report-rail-section-body">{children}</div>}
    </section>
  );
}

function OutlinePanel({
  outline,
  unavailable,
  bodyOnly,
}: {
  outline: ReportOutlineItem[];
  unavailable: boolean;
  bodyOnly: boolean;
}) {
  if (unavailable) {
    return (
      <div className="report-rail-placeholder">
        Outline unavailable for this report version.
      </div>
    );
  }
  if (bodyOnly) {
    return (
      <div className="report-rail-placeholder">
        Outline navigation requires structured report blocks.
      </div>
    );
  }
  if (outline.length === 0) {
    return <div className="report-rail-placeholder">No sections yet.</div>;
  }

  const outlineLink = (
    blockId: string,
    children: ReactNode,
    ariaLabel?: string,
  ) => (
    <a
      href={`#${encodeURIComponent(blockId)}`}
      aria-label={ariaLabel}
      onClick={(event) => {
        event.preventDefault();
        window.history.replaceState(null, '', `#${encodeURIComponent(blockId)}`);
        revealReportBlock(blockId);
      }}
    >
      {children}
    </a>
  );

  return (
    <ol className="report-outline-list">
      {outline.map((item) => (
        <li key={`${item.blockId}:${item.number}`}>
          {outlineLink(
            item.blockId,
            <>
              <span className="report-outline-number">
                {String(item.number).padStart(2, '0')}
              </span>
              <span>{item.label}</span>
            </>,
            `${String(item.number).padStart(2, '0')} ${item.label}`,
          )}
          {item.children.length > 0 && (
            <ul>
              {item.children.map((child) => (
                <li key={child.blockId}>
                  {outlineLink(child.blockId, child.label)}
                </li>
              ))}
            </ul>
          )}
        </li>
      ))}
    </ol>
  );
}

function InterimOutlinksPanel({
  outlinks,
  waves,
}: {
  outlinks: ReturnType<typeof deriveInterimReportOutlinks>;
  waves: Array<{ id: string; title: string }> | undefined;
}) {
  if (outlinks.length === 0) {
    return <div className="report-rail-placeholder">No referenced documents.</div>;
  }

  return (
    <ul className="report-outlinks-list">
      {outlinks.map((outlink) => {
        const target = waves?.find((candidate) => candidate.id === outlink.waveId);
        return (
          <li key={outlink.waveId}>
            {target ? (
              <Link
                to="/wave/$waveId"
                params={{ waveId: outlink.waveId }}
                hash={outlink.blockId}
              >
                {waveDisplayTitle(target.title)}
              </Link>
            ) : (
              <span className="report-outlink--dead" title="Referenced wave not found">
                {outlink.waveId}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function BacklinkQuote({ backlink }: { backlink: WaveBacklink }) {
  const quote = backlink.quote;
  if (!quote) return <>{backlink.label}</>;
  return (
    <>
      {quote.head_elided && '…'}
      {quote.before}
      {quote.label !== '' && <b>{quote.label}</b>}
      {quote.after}
      {quote.tail_elided && '…'}
    </>
  );
}

function BacklinksPanel({
  waveId: currentWaveId,
  hasRenderedBlocks,
  page,
  error,
}: {
  waveId: string;
  hasRenderedBlocks: boolean;
  page?: WaveBacklinksResponse;
  error: Error | null;
}) {
  const groups = useMemo(() => {
    const backlinks = page?.backlinks ?? [];
    const grouped = new Map<
      string,
      { title: string; entries: WaveBacklink[] }
    >();
    for (const backlink of backlinks) {
      const group = grouped.get(backlink.src_wave_id);
      if (group) group.entries.push(backlink);
      else {
        grouped.set(backlink.src_wave_id, {
          title:
            backlink.src_wave_id === currentWaveId
              ? 'This wave (self-reference)'
              : backlink.src_wave_title,
          entries: [backlink],
        });
      }
    }
    return [...grouped.entries()];
  }, [page?.backlinks, currentWaveId]);

  return (
    <div className="report-backlinks">
      {error && (
        <div role="alert" className="report-error">
          Could not load backlinks: {formatApiError(error)}
        </div>
      )}
      {page?.truncated && (
        <p role="status">Some backlinks are not shown.</p>
      )}
      {!!page?.skipped_sources && (
        <p role="status">
          Backlinks from {page.skipped_sources} source report
          {page.skipped_sources === 1 ? '' : 's'} could not be loaded.
        </p>
      )}
      {groups.length === 0 && !error && !page?.truncated &&
        !page?.skipped_sources && (
        <div className="report-rail-placeholder">No backlinks yet.</div>
      )}
      {groups.map(([waveId, group]) => (
        <div className="report-backlinks-group" key={waveId}>
          <h3>{group.title}</h3>
          <ul>
            {group.entries.map((entry, index) => (
              <li
                key={`${entry.src_block_id}:${entry.dst_block_id ?? ''}:${index}`}
              >
                <Link
                  to="/wave/$waveId"
                  params={{ waveId }}
                  hash={entry.src_block_id}
                >
                  <BacklinkQuote backlink={entry} />
                </Link>
                {hasRenderedBlocks && entry.dst_block_id && (
                  <span className="report-backlinks-target">
                    {' '}
                    · cites block {entry.dst_block_id}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      ))}
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
  const [outlinksCollapsed, setOutlinksCollapsed] = useState(() =>
    readRailSectionCollapsed(REPORT_OUTLINKS_COLLAPSED_STORAGE_KEY),
  );
  const [backlinksCollapsed, setBacklinksCollapsed] = useState(() =>
    readRailSectionCollapsed(REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY),
  );
  const [filesCollapsed, setFilesCollapsed] = useState(() =>
    readRailSectionCollapsed(REPORT_FILES_COLLAPSED_STORAGE_KEY),
  );
  const [showHiddenFiles, setShowHiddenFiles] = useState(false);
  const backlinksQ = useWaveBacklinksQuery(wave.id);
  const wavesByCoveQ = useWavesByCoveQuery(wave.coveId);
  const outline = useMemo(
    () => deriveOutline(reportCard?.blocks),
    [reportCard?.blocks],
  );
  const outlinks = useMemo(
    () => deriveInterimReportOutlinks(reportCard?.blocks),
    [reportCard?.blocks],
  );

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
      writeCollapsedState(REPORT_RAIL_COLLAPSED_STORAGE_KEY, next);
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
      <aside
        className={
          'report-rail' + (reportRailCollapsed ? ' report-rail--collapsed' : '')
        }
        aria-label="Report context"
      >
        <section
          className="report-rail-section report-rail-section--outline"
          aria-label="Outline"
        >
          <header className="report-rail-head report-rail-head--top">
            {!reportRailCollapsed && (
              <>
                <h2>Outline</h2>
                <span className="report-rail-count">{outline.length}</span>
              </>
            )}
            <div className="report-rail-actions">{railCollapseButton}</div>
          </header>
          {!reportRailCollapsed && (
            <div className="report-rail-section-body">
              <OutlinePanel
                outline={outline}
                unavailable={reportCard?.unsupportedVersion != null}
                bodyOnly={hasReportCard && reportCard?.blocks == null}
              />
            </div>
          )}
        </section>
        {!reportRailCollapsed && (
          <>
            <RailSection
              title="Referenced documents"
              count={outlinks.length}
              collapsed={outlinksCollapsed}
              onCollapsedChange={(collapsed) => {
                setOutlinksCollapsed(collapsed);
                writeCollapsedState(
                  REPORT_OUTLINKS_COLLAPSED_STORAGE_KEY,
                  collapsed,
                );
              }}
            >
              <InterimOutlinksPanel
                outlinks={outlinks}
                waves={wavesByCoveQ.data}
              />
            </RailSection>
            <RailSection
              title="Backlinks"
              count={backlinksQ.data?.backlinks.length ?? 0}
              collapsed={backlinksCollapsed}
              onCollapsedChange={(collapsed) => {
                setBacklinksCollapsed(collapsed);
                writeCollapsedState(
                  REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY,
                  collapsed,
                );
              }}
            >
              <BacklinksPanel
                waveId={wave.id}
                hasRenderedBlocks={reportCard?.blocks != null}
                page={backlinksQ.data}
                error={backlinksQ.error}
              />
            </RailSection>
            <RailSection
              title="Files"
              collapsed={filesCollapsed}
              onCollapsedChange={(collapsed) => {
                setFilesCollapsed(collapsed);
                writeCollapsedState(
                  REPORT_FILES_COLLAPSED_STORAGE_KEY,
                  collapsed,
                );
              }}
              actions={
                !filesCollapsed && (
                  <button
                    type="button"
                    className="report-rail-toggle report-rail-toggle--show-all"
                    aria-pressed={showHiddenFiles}
                    onClick={toggleHiddenFiles}
                  >
                    Show all
                  </button>
                )
              }
            >
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
            </RailSection>
          </>
        )}
      </aside>
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
            {selectedFilePath === 'report.md' &&
            reportCard?.unsupportedVersion != null ? (
              <UnsupportedReportVersionState />
            ) : hasReportCard || selectedFilePath !== 'report.md' ? (
              <div className="report-body">
                <ReportContent
                  waveId={wave.id}
                  path={selectedFilePath}
                  reportCardBody={reportCard?.body}
                  reportCardBlocks={reportCard?.blocks}
                />
              </div>
            ) : (
              <ReportEmptyState />
            )}
          </article>
        </SpecConversation>
      </section>
    </div>
  );
}
