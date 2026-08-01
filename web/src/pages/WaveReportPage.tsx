import {
  lazy,
  Suspense,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type ReactNode,
} from 'react';
import { Link, useRouterState } from '@tanstack/react-router';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../api/calm';
import {
  useWaveBacklinksQuery,
  useWaveFileContent,
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
import { SpecConversation } from './SpecConversation';
import { ChevronIcon } from '../shared/components/ChevronIcon';
import { deriveOutline, type ReportOutlineItem } from './report-outline';

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
const REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:backlinks:collapsed';
const REPORT_FILES_COLLAPSED_STORAGE_KEY = 'calm:report-rail:files:collapsed';
const REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY =
  'calm:report-conversation:collapsed';

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

function readConversationCollapsed(): boolean {
  if (typeof window === 'undefined') return true;
  try {
    const stored = window.localStorage.getItem(
      REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY,
    );
    return stored == null ? true : stored === 'true';
  } catch {
    return true;
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
  const target = document.getElementById(blockId);
  const block = target?.closest('.report-block');
  if (!target || !block) return;
  target.scrollIntoView();
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
    <section
      className="report-rail-section"
      aria-label={title}
      data-collapsed={collapsed}
    >
      <div className="report-rail-section-head">
        <h2>
          <button
            type="button"
            className="report-rail-head"
            aria-expanded={!collapsed}
            aria-label={title}
            onClick={() => onCollapsedChange(!collapsed)}
          >
            <ChevronIcon />
            <span className="report-rail-heading">{title}</span>
            {count != null && <span className="report-rail-count">{count}</span>}
          </button>
        </h2>
        {!collapsed && actions && (
          <div className="report-rail-actions">{actions}</div>
        )}
      </div>
      <div className="report-rail-section-body">{children}</div>
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
        // Deliberately bypass TanStack Router: this local scroll marker does
        // not need to trigger route state or the deep-link arrival effect.
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
        <li key={item.blockId}>
          {outlineLink(
            item.blockId,
            <>
              {item.number !== null && (
                <span className="report-outline-number">
                  {String(item.number).padStart(2, '0')}
                </span>
              )}
              <span>{item.label}</span>
            </>,
            item.number === null
              ? item.label
              : `${String(item.number).padStart(2, '0')} ${item.label}`,
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
                  <span className="report-backlinks-title">
                    {group.title}
                  </span>
                  {' '}
                  <span className="report-backlinks-quote">
                    <BacklinkQuote backlink={entry} />
                  </span>
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
  const [lastWaveId, setLastWaveId] = useState<string>(wave.id);
  const [reportRailCollapsed, setReportRailCollapsed] = useState(
    () => readReportRailCollapsed(),
  );
  const [backlinksCollapsed, setBacklinksCollapsed] = useState(() =>
    readRailSectionCollapsed(REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY),
  );
  const [filesCollapsed, setFilesCollapsed] = useState(() =>
    readRailSectionCollapsed(REPORT_FILES_COLLAPSED_STORAGE_KEY),
  );
  const [conversationCollapsed, setConversationCollapsed] = useState(() =>
    readConversationCollapsed(),
  );
  const [showHiddenFiles, setShowHiddenFiles] = useState(false);
  const railCollapseButtonRef = useRef<HTMLButtonElement>(null);
  const railOpenButtonRef = useRef<HTMLButtonElement>(null);
  const railFocusTransferPending = useRef(false);
  const backlinksQ = useWaveBacklinksQuery(wave.id);
  const outline = useMemo(
    () => deriveOutline(reportCard?.blocks),
    [reportCard?.blocks],
  );

  // Sync reset during render so a new wave never renders with the old file path.
  if (lastWaveId !== wave.id) {
    setLastWaveId(wave.id);
    setSelectedFilePath('report.md');
  }

  const toggleReportRailCollapsed = () => {
    railFocusTransferPending.current = true;
    setReportRailCollapsed((current) => {
      const next = !current;
      writeCollapsedState(REPORT_RAIL_COLLAPSED_STORAGE_KEY, next);
      return next;
    });
  };
  useLayoutEffect(() => {
    if (!railFocusTransferPending.current) return;
    railFocusTransferPending.current = false;
    if (reportRailCollapsed) railOpenButtonRef.current?.focus();
    else railCollapseButtonRef.current?.focus();
  }, [reportRailCollapsed]);
  const toggleHiddenFiles = () => {
    setShowHiddenFiles((current) => !current);
  };
  const toggleConversationCollapsed = () => {
    setConversationCollapsed((current) => {
      const next = !current;
      writeCollapsedState(REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY, next);
      return next;
    });
  };
  const conversationOpen = !conversationCollapsed;

  const railCollapseButton = (
    <button
      type="button"
      ref={railCollapseButtonRef}
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
        'report-page' +
        (reportRailCollapsed ? ' report-page--rail-collapsed' : '') +
        (conversationOpen ? ' report-page--conversation-open' : '')
      }
    >
      <aside
        id="report-context-rail"
        className={
          'report-rail' + (reportRailCollapsed ? ' report-rail--collapsed' : '')
        }
        hidden={reportRailCollapsed}
        aria-label="Report context"
      >
        <div className="report-rail-top">
          <span>Document</span>
          {railCollapseButton}
        </div>
        <section
          className="report-rail-section report-rail-section--outline"
          aria-label="Outline"
        >
          <h2 className="report-rail-head report-rail-head--static">
            <span className="report-rail-heading">Outline</span>
            <span className="report-rail-count">{outline.length}</span>
          </h2>
          <div className="report-rail-section-body">
            <OutlinePanel
              outline={outline}
              unavailable={reportCard?.unsupportedVersion != null}
              bodyOnly={hasReportCard && reportCard?.blocks == null}
            />
          </div>
        </section>
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
            writeCollapsedState(REPORT_FILES_COLLAPSED_STORAGE_KEY, collapsed);
          }}
          actions={
            <button
              type="button"
              className="report-rail-toggle report-rail-toggle--show-all"
              aria-pressed={showHiddenFiles}
              onClick={toggleHiddenFiles}
            >
              Show all
            </button>
          }
        >
          <div className="report-rail-files">
            <WaveFileTree
              waveId={wave.id}
              selectedPath={selectedFilePath}
              onSelectedPathChange={(path) => {
                setSelectedFilePath(path ?? 'report.md');
              }}
              ariaLabel="Wave files"
              enabled={!filesCollapsed}
              showHidden={showHiddenFiles}
              fallback={
                <div className="report-rail-placeholder">No files yet.</div>
              }
            />
          </div>
        </RailSection>
      </aside>
      <section className="report-center" aria-label="Report">
        <button
          type="button"
          ref={railOpenButtonRef}
          className="report-rail-open"
          hidden={!reportRailCollapsed}
          onClick={toggleReportRailCollapsed}
          aria-label="Expand report rail"
          aria-controls="report-context-rail"
          title="Expand report rail"
        >
          <ChevronIcon />
        </button>
        <div
          className="report-document-scroll"
          // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable report document must be keyboard-focusable.
          tabIndex={0}
          aria-label="Report document"
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
        </div>
      </section>
      <aside
        className={
          'report-conversation-drawer' +
          (conversationOpen ? ' report-conversation-drawer--open' : '')
        }
        aria-label="Conversation drawer"
      >
        <button
          type="button"
          className="report-conversation-toggle"
          aria-controls="report-conversation-panel"
          aria-expanded={conversationOpen}
          aria-label={
            conversationOpen
              ? 'Close conversation drawer'
              : 'Open conversation drawer'
          }
          title={conversationOpen ? 'Close conversation' : 'Open conversation'}
          onClick={toggleConversationCollapsed}
        >
          <ChevronIcon />
        </button>
        <div
          id="report-conversation-panel"
          className="report-conversation-drawer-panel"
          aria-hidden={!conversationOpen}
        >
          <SpecConversation
            specCardId={specCardId}
            drawerOpen={conversationOpen}
            onClose={toggleConversationCollapsed}
          />
        </div>
      </aside>
    </div>
  );
}
