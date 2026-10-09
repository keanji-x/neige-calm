import { useCompactViewport } from '../../../ui/viewport/public.ts';
// The report document — the main column on a track and an area: a sequence of typed blocks.

import { useEffect, useMemo, type ReactNode } from 'react';

import { parse, sanitizeAstPolicy } from '../../../../../core/markdown/public.ts';
import { ProseBlock, PlainReportText } from './content.tsx';
export { ProseBlock } from './content.tsx';
import { reportReferenceRepository, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
import {
  deriveReportTasks, isTaskBlock,
  type PreviewResolution, type ReportBlock, type ReportLinkTarget, type ReportTaskRow, type TaskVerdict,
  type TrackReport,
} from '../../../../../core/domain/report.ts';
import {
  type ReportFileLinkTarget,
} from '../../../../../core/domain/report-file.ts';
import type { SeriesResolution } from '../../../../../core/domain/report-series.ts';
import { type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { type ReportLinkPreviewResources } from '../link-preview/public.tsx';
import { ReportDetails } from './details.tsx';
import { revealReportAnchor } from '../anchor/public.ts';
import { ReportAppBlock } from '../app/public.tsx';
import { ReportCandlesBlock } from '../candles/public.tsx';
import { ReportPreviewBlock, type PreviewViewportStore } from '../preview/public.tsx';
import { ReportSeriesBlock } from '../series/public.tsx';
import { ReportTableBlock } from '../table/public.tsx';
import { NativeReportView } from '../native/public.tsx';
import { ReportTaskBlock } from '../task/public.tsx';
import styles from './document.module.css';

export type ReportDocumentProps = Readonly<{
  /** The track's report. `null` when it has none. */
  report: TrackReport | null;
  /** What the empty state should offer, which differs per route. */
  empty: ReactNode;
  /** The outline rail, which hangs in the document's leading gutter. */
  rail?: ReactNode;
  /** The byline row (`Planner Agent · 2h`), composed by `app/router`. */
  byline?: ReactNode;
  /** How many backlinks land on each block, for the sidenote markers. */
  backlinkCounts?: ReadonlyMap<string, number>;
  /** A `neige://wave/…` link was activated. Absent ⇒ links render as plain text. */
  onOpenLink?: (target: ReportLinkTarget) => void;
  /** A file link admitted beneath `fileRoot` was activated. */
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  /** A `neige://source/…` citation was activated. Absent ⇒ citations render as an inline badge plus their label. */
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  /** Absolute root used to admit relative or already-absolute workspace links. */
  fileRoot?: string;
  /** Workspace-relative directory containing the Markdown currently rendered. */
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
  /** The anchor the reader arrived at, from a deep link or a backlink. */
  arrivalAnchorId?: string | null;
  /** The same execution diagnostics used by the task inventory. */
  taskVerdicts?: readonly TaskVerdict[];
  /** App-composed rows shared with the task inventory when execution evidence is loaded. */
  taskRows?: readonly ReportTaskRow[];
  /** App-owned current/history query, scoped to a task. */
  renderTaskExecution?: (task: ReportTaskRow, expanded: boolean) => ReactNode;
  /** Resolves a Track overlay for a live `table` or a `view` live slot; each renderer validates its declared contract. */
  resolveOverlay?: (source: string) => unknown;
  /** Resolves a `chart.series` block, by id and rev, to the app's query of the kernel's resolved data. Absent ⇒ series blocks say the view carries no such data. */
  resolveSeries?: (blockId: string, rev: number) => SeriesResolution | undefined;
  /** Resolves a `preview` block's `key` to the app's read of the track's registered previews. Absent ⇒ preview blocks say the view carries none. */
  resolvePreview?: (key: string) => PreviewResolution;
  /** Where preview blocks remember the reader's device choice, by block key (the app scopes it per track). Absent ⇒ not remembered. */
  previewViewports?: PreviewViewportStore;
}>;

/** Agent-authored destinations become admitted preview controls. Navigation stays behind typed callbacks or explicit external activation. */
export function ReportDocument({
  report, empty, rail, byline, backlinkCounts, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview,
  resolveOverlay, resolveSeries, resolvePreview, previewViewports,
  arrivalAnchorId, taskVerdicts, taskRows, renderTaskExecution,
}: ReportDocumentProps) {
  const compactViewport = useCompactViewport();
  const referenceRepository = useMemo(() => reportReferenceRepository(report === null ? [] : report.blocks === null
    ? [report.body || report.summary] : report.blocks.flatMap(block => block.kind === 'prose' ? [block.payload.markdown] : [])), [report]);
  useEffect(() => {
    if (arrivalAnchorId === null || arrivalAnchorId === undefined) return;
    revealReportAnchor(arrivalAnchorId);
  }, [arrivalAnchorId]);

  if (report === null) return <>{empty}</>;

  const prose = report.blocks === null ? [report.body || report.summary]
    : report.blocks.every((block) => block.kind === 'prose') ? report.blocks.map((block) => block.payload.markdown) : null;
  const outlineOnly = compactViewport && prose !== null && prose.every((markdown) => {
    const parsed = parse(markdown);
    return parsed.status === 'ready' && sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' }).children.every((block) => block.type === 'heading');
  });

  return (
    <article className={`calm-prose ${styles.doc} ${outlineOnly ? styles.outlineOnly : ''}`} data-nc-report="" tabIndex={-1}>
      {outlineOnly && <div className={styles.mobileEmpty}><h2>从一个想法开始</h2><p>计划、进展与重要结论会整理在这里。</p></div>}
      {rail}
      {byline !== undefined && <div className={styles.byline}>{byline}</div>}
      {report.blocks === null
        ? (
          <div className={styles.row}>
            <div className={styles.block} data-nc-report-reading="">
              <ProseBlock
                markdown={report.body || report.summary}
                blockId={null}
                referenceRepository={referenceRepository} onOpenLink={onOpenLink}
                onOpenFileLink={onOpenFileLink}
                onOpenSourceLink={onOpenSourceLink}
                fileRoot={fileRoot}
                fileBasePath={fileBasePath}
                linkPreview={linkPreview}
              />
            </div>
          </div>
        )
        : (() => {
          const documentBlocks = report.blocks.filter((block) => !isProcessBlock(block));
          const processBlocks = report.blocks.filter(isProcessBlock);
          return (
            <>
              {documentBlocks.map((block) => (
                <BlockSlot
                  key={block.id}
                  block={block}
                  backlinks={backlinkCounts?.get(block.id) ?? 0}
                  referenceRepository={referenceRepository} onOpenLink={onOpenLink}
                  onOpenFileLink={onOpenFileLink}
                  onOpenSourceLink={onOpenSourceLink}
                  fileRoot={fileRoot}
                  fileBasePath={fileBasePath}
                  linkPreview={linkPreview}
                  resolveOverlay={resolveOverlay}
                  resolveSeries={resolveSeries}
                  resolvePreview={resolvePreview}
                  previewViewports={previewViewports}
                />
              ))}
              {processBlocks.length > 0 && (
                <ReportReference blocks={processBlocks} backlinkCounts={backlinkCounts}
                  tasks={taskRows ?? deriveReportTasks(report.blocks, taskVerdicts)} renderTaskExecution={renderTaskExecution}
                  referenceRepository={referenceRepository} onOpenLink={onOpenLink} onOpenFileLink={onOpenFileLink}
                  onOpenSourceLink={onOpenSourceLink} fileRoot={fileRoot} fileBasePath={fileBasePath} linkPreview={linkPreview} />
              )}
            </>
          );
        })()}
    </article>
  );
}

function isProcessBlock(block: ReportBlock): boolean {
  return isTaskBlock(block);
}

function ReportReference({ blocks, backlinkCounts, tasks, renderTaskExecution, ...textContext }: {
  blocks: readonly ReportBlock[];
  backlinkCounts?: ReadonlyMap<string, number>;
  tasks: readonly ReportTaskRow[];
  referenceRepository: ReferenceRepository | null;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
  renderTaskExecution?: ReportDocumentProps['renderTaskExecution'];
}) {
  const tasksByBlock = new Map(tasks.map((task) => [task.blockId, task]));
  return (
    <div className={styles.row}>
      <ReportDetails title="Reference" meta={`${blocks.length} ${blocks.length === 1 ? 'task' : 'tasks'}`} layout="grid" reference>
        {/* Not `BlockSlot`: a slot is `display: contents` over the article's grid, and this `<details>` is not that grid. */}
        {blocks.map((block) => {
          const backlinks = backlinkCounts?.get(block.id) ?? 0;
          return (
            <div key={block.id} className={styles.referenceItem} id={block.id}>
              <BlockBody block={block} task={tasksByBlock.get(block.id)} renderTaskExecution={renderTaskExecution} {...textContext} />
              {backlinks > 0 && (
                <span
                  className={styles.referenceSidenote}
                  title={`${backlinks} report${backlinks === 1 ? '' : 's'} cite this block`}
                >
                  ◂ {backlinks}
                </span>
              )}
            </div>
          );
        })}
      </ReportDetails>
    </div>
  );
}

/** One block, plus the sidenote that belongs to it. */
function BlockSlot({
  block, backlinks, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview, referenceRepository, resolveOverlay, resolveSeries,
  resolvePreview, previewViewports,
}: {
  referenceRepository: ReferenceRepository | null;
  block: ReportBlock;
  backlinks: number;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
  resolveOverlay?: ReportDocumentProps['resolveOverlay'];
  resolveSeries?: ReportDocumentProps['resolveSeries'];
  resolvePreview?: ReportDocumentProps['resolvePreview'];
  previewViewports?: PreviewViewportStore;
}) {
  return (
    <div className={styles.row}>
      <div className={styles.block} id={block.id} data-nc-report-reading="">
        {block.kind === 'prose'
          ? <ProseBlock
              markdown={block.payload.markdown}
              blockId={block.id}
              referenceRepository={referenceRepository} onOpenLink={onOpenLink}
              onOpenFileLink={onOpenFileLink}
              onOpenSourceLink={onOpenSourceLink}
              fileRoot={fileRoot}
              fileBasePath={fileBasePath}
              linkPreview={linkPreview}
            />
          : <BlockBody block={block} referenceRepository={referenceRepository} onOpenLink={onOpenLink}
              onOpenFileLink={onOpenFileLink} fileRoot={fileRoot} fileBasePath={fileBasePath}
              onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview}
              resolveOverlay={resolveOverlay} resolveSeries={resolveSeries} resolvePreview={resolvePreview}
              previewViewports={previewViewports} />}
      </div>
      {backlinks > 0 && (
        <span className={styles.sidenote} title={`${backlinks} report${backlinks === 1 ? '' : 's'} cite this block`}>
          ◂ {backlinks}
        </span>
      )}
    </div>
  );
}

/** One bad block may not cost the page: an unknown kind or an unparsable payload degrades to one line. */
function BlockBody({
  block, task, renderTaskExecution, referenceRepository, onOpenLink, onOpenFileLink, fileRoot, fileBasePath, onOpenSourceLink, linkPreview, resolveOverlay, resolveSeries, resolvePreview, previewViewports,
}: {
  referenceRepository: ReferenceRepository | null;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  block: ReportBlock; task?: ReportTaskRow; renderTaskExecution?: ReportDocumentProps['renderTaskExecution'];
  /** A table cell that is one source citation is the same control the prose paints. */
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  linkPreview?: ReportLinkPreviewResources;
  resolveOverlay?: ReportDocumentProps['resolveOverlay'];
  resolveSeries?: ReportDocumentProps['resolveSeries'];
  resolvePreview?: ReportDocumentProps['resolvePreview'];
  previewViewports?: PreviewViewportStore;
}): ReactNode {
  const textContext = { referenceRepository, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview };
  const renderText = (text: string) => <PlainReportText text={text} context={textContext} />;
  switch (block.kind) {
    case 'table':
      return <ReportTableBlock renderText={renderText} payload={block.payload} resolveLive={resolveOverlay} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
    case 'view': return <NativeReportView renderText={renderText} payload={block.payload} resolveOverlay={resolveOverlay} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
    case 'chart.candles': return <ReportCandlesBlock payload={block.payload} />;
    case 'chart.series':
      return <ReportSeriesBlock payload={block.payload} blockId={block.id} rev={block.rev} resolve={resolveSeries} />;
    case 'task': return <ReportTaskBlock payload={block.payload} blockId={block.id} task={task} renderExecution={renderTaskExecution}
      textContext={textContext} />;
    case 'app': return <ReportAppBlock payload={block.payload} />;
    case 'preview': return <ReportPreviewBlock payload={block.payload} resolve={resolvePreview}
      viewports={previewViewports} />;
    case 'unsupported':
      return (
        <div className={styles.unsupported} role="note">
          unsupported block kind {block.declaredKind}
        </div>
      );
    // `prose` is handled by the slot, which owns the markdown pipeline.
    case 'prose': return null;
  }
}
