import { ReadOnlyCode } from '../../../ui/code/public.tsx';

import { useCompactViewport } from '../../../ui/viewport/public.ts';
// The report document — the main column on a track and an area: a sequence of typed blocks.

import { useEffect, type ReactNode } from 'react';

import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy, sanitizeAstPolicy,
  type SafeBlock, type SafeInline,
} from '../../../../../core/markdown/public.ts';
import {
  deriveReportTasks, isTaskBlock, parseReportLink,
  type PreviewResolution, type ReportBlock, type ReportLinkTarget, type ReportTaskRow, type TaskVerdict,
  type TrackReport,
} from '../../../../../core/domain/report.ts';
import {
  parseReportFileLink, reportFilePathRelativeToRoot, type ReportFileLinkTarget,
} from '../../../../../core/domain/report-file.ts';
import type { SeriesResolution } from '../../../../../core/domain/report-series.ts';
import { parseReportSourceLink, type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { ReportLinkPreview, ReportSourceLinkPreview, externalPreviewUrl, type ReportLinkPreviewResources, type PreviewDestination } from '../link-preview/public.tsx';
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
                onOpenLink={onOpenLink}
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
                  onOpenLink={onOpenLink}
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
                  tasks={taskRows ?? deriveReportTasks(report.blocks, taskVerdicts)} renderTaskExecution={renderTaskExecution} />
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

function ReportReference({ blocks, backlinkCounts, tasks, renderTaskExecution }: {
  blocks: readonly ReportBlock[];
  backlinkCounts?: ReadonlyMap<string, number>;
  tasks: readonly ReportTaskRow[];
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
              <BlockBody block={block} task={tasksByBlock.get(block.id)} renderTaskExecution={renderTaskExecution} />
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
  block, backlinks, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview, resolveOverlay, resolveSeries,
  resolvePreview, previewViewports,
}: {
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
              onOpenLink={onOpenLink}
              onOpenFileLink={onOpenFileLink}
              onOpenSourceLink={onOpenSourceLink}
              fileRoot={fileRoot}
              fileBasePath={fileBasePath}
              linkPreview={linkPreview}
            />
          : <BlockBody block={block} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview}
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
  block, task, renderTaskExecution, onOpenSourceLink, linkPreview, resolveOverlay, resolveSeries, resolvePreview, previewViewports,
}: {
  block: ReportBlock; task?: ReportTaskRow; renderTaskExecution?: ReportDocumentProps['renderTaskExecution'];
  /** A table cell that is one source citation is the same control the prose paints. */
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  linkPreview?: ReportLinkPreviewResources;
  resolveOverlay?: ReportDocumentProps['resolveOverlay'];
  resolveSeries?: ReportDocumentProps['resolveSeries'];
  resolvePreview?: ReportDocumentProps['resolvePreview'];
  previewViewports?: PreviewViewportStore;
}): ReactNode {
  switch (block.kind) {
    case 'table':
      return <ReportTableBlock payload={block.payload} resolveLive={resolveOverlay} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
    case 'view': return <NativeReportView payload={block.payload} resolveOverlay={resolveOverlay} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
    case 'chart.candles': return <ReportCandlesBlock payload={block.payload} />;
    case 'chart.series':
      return <ReportSeriesBlock payload={block.payload} blockId={block.id} rev={block.rev} resolve={resolveSeries} />;
    case 'task': return <ReportTaskBlock payload={block.payload} blockId={block.id} task={task} renderExecution={renderTaskExecution} />;
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

/** The rendered-Markdown half of a report block. Exported for the recipe editor. */
export function ProseBlock({
  markdown, blockId, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview,
}: {
  markdown: string;
  blockId: string | null;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
}) {
  const parsed = parse(markdown);
  if (parsed.status === 'failed') {
    return <pre className={styles.raw}>{markdown}</pre>;
  }

  // Heading ids come from the same `extractOutline` call the outline uses, so the two cannot drift.
  const headingIds = blockId === null
    ? new Map<number, string>()
    : new Map(extractOutline([{ context: { blockId }, ast: parsed.value }], {
      maxDepth: REPORT_MAX_DEPTH,
      headingId: reportHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'visible',
      traversal: 'recursive',
    }).map((heading) => [heading.position.start.offset ?? -1, heading.id]));

  const ast = sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' });
  if (ast.children.length === 0) return null;
  return <>{ast.children.map((block, index) => (
    <Block
      key={index}
      block={block}
      headingIds={headingIds}
      onOpenLink={onOpenLink}
      onOpenFileLink={onOpenFileLink}
      onOpenSourceLink={onOpenSourceLink}
      fileRoot={fileRoot}
      fileBasePath={fileBasePath}
      linkPreview={linkPreview}
    />
  ))}</>;
}

type BlockContext = Readonly<{
  inLink?: boolean;
  headingIds: ReadonlyMap<number, string>;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
}>;

function Block({ block, headingIds, ...rest }: { block: SafeBlock } & BlockContext): ReactNode {
  const context: BlockContext = { headingIds, ...rest };
  switch (block.type) {
    case 'heading': {
      // H1 is a section rule, not a page title; anything deeper than H2 renders as H2 rather than vanishing.
      const id = headingIds.get(block.position.start.offset ?? -1);
      if (block.depth !== 1) {
        return (
          <h3 className={styles.h2} id={id}>
            <Inlines nodes={block.children} {...context} />
          </h3>
        );
      }
      return (
        <h2 className={styles.h1} id={id}>
          <span className={styles.headingText}><Inlines nodes={block.children} {...context} /></span>
        </h2>
      );
    }
    case 'paragraph':
      return <p className={styles.p}><Inlines nodes={block.children} {...context} /></p>;
    case 'code':
      return <ReadOnlyCode text={block.value} source={{ kind: 'language', value: block.language ?? '' }} />;
    case 'blockquote':
      return (
        <blockquote className={styles.quote}>
          {block.children.map((child, index) => <Block key={index} block={child} {...context} />)}
        </blockquote>
      );
    case 'list': {
      const Tag = block.ordered ? 'ol' : 'ul';
      return (
        <Tag className={styles.list} start={block.ordered && block.start !== null ? block.start : undefined}>
          {block.children.map((item, index) => (
            <li key={index} className={styles.item}>
              {item.checked !== null && (
                <input type="checkbox" className={styles.check} checked={item.checked} disabled readOnly />
              )}
              {item.children.map((child, childIndex) => (
                !block.spread && child.type === 'paragraph'
                  ? <Inlines key={childIndex} nodes={child.children} {...context} />
                  : <Block key={childIndex} block={child} {...context} />
              ))}
            </li>
          ))}
        </Tag>
      );
    }
    case 'table':
      return (
        // Its own scroll container: a wide table may not make the page scroll sideways.
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <tbody>
              {block.children.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.children.map((cell, cellIndex) => {
                    const Cell = rowIndex === 0 ? 'th' : 'td';
                    return (
                      <Cell
                        key={cellIndex}
                        className={styles.cell}
                        style={{ textAlign: block.align[cellIndex] ?? undefined }}
                      >
                        <Inlines nodes={cell.children} {...context} />
                      </Cell>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case 'thematicBreak':
      return <hr className={styles.rule} />;
  }
}

function Inlines({ nodes, ...context }: { nodes: readonly SafeInline[] } & BlockContext): ReactNode {
  return <>{nodes.map((node, index) => <Inline key={index} node={node} {...context} />)}</>;
}

function Inline({ node, ...context }: { node: SafeInline } & BlockContext): ReactNode {
  const { onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview } = context;
  switch (node.type) {
    case 'text':
      return node.value;
    case 'inlineCode':
      return <code className={styles.inlineCode}>{node.value}</code>;
    case 'strong':
      return <strong className={styles.strong}><Inlines nodes={node.children} {...context} /></strong>;
    case 'emphasis':
      return <em><Inlines nodes={node.children} {...context} /></em>;
    case 'delete':
      return <del><Inlines nodes={node.children} {...context} /></del>;
    case 'break':
      return <br />;
    case 'link':
    case 'image': {
      const image = node.type === 'image';
      const label = image ? node.alt || 'Image' : inlineLabel(node.children);
      const body = image ? <span className={styles.imageAlt}>{node.alt || 'Image'}</span>
        : <Inlines nodes={node.children} {...context} inLink />;
      if (context.inLink) return body;
      const renderMarkdown = (text: string, basePath?: string) => <ProseBlock
        markdown={text} blockId={null} {...context} fileBasePath={basePath ?? fileBasePath} />;
      const wrap = (destination: PreviewDestination, trigger: (activate: () => void, dismiss: () => void) => ReactNode, onOpen?: () => void) => (
        <ReportLinkPreview destination={destination} resources={linkPreview} label={label}
          trigger={trigger} onOpen={onOpen} renderMarkdown={renderMarkdown} />
      );
      const target = image ? null : parseReportLink(node.destination);
      if (target !== null && (onOpenLink !== undefined || linkPreview !== undefined)) {
        const open = onOpenLink === undefined ? undefined : () => onOpenLink(target);
        return wrap({ kind: 'reference', destination: node.destination, target },
          (activate, dismiss) => <button type="button" className={styles.link} onClick={open === undefined ? activate : () => { dismiss(); open(); }}>{body}</button>, open);
      }
      const sourceTarget = image ? null : parseReportSourceLink(node.destination);
      if (sourceTarget !== null) {
        return <ReportSourceLinkPreview target={sourceTarget} label={label} resources={linkPreview}
          onOpen={onOpenSourceLink}>{body}</ReportSourceLinkPreview>;
      }
      const fileTarget = parseReportFileLink(node.destination);
      const path = fileTarget !== null && fileRoot !== undefined
        ? reportFilePathRelativeToRoot(fileRoot, fileTarget, fileBasePath) : null;
      if (path !== null && (onOpenFileLink !== undefined || linkPreview !== undefined)) {
        const open = onOpenFileLink === undefined ? undefined : () => onOpenFileLink({ path });
        return wrap({ kind: 'file', path },
          (activate, dismiss) => <button type="button" className={styles.link} title={path} onClick={open === undefined ? activate : () => { dismiss(); open(); }}>{body}</button>, open);
      }
      const url = externalPreviewUrl(node.destination);
      if (url !== null) {
        return wrap({ kind: 'web', url, image: image || /\.(png|jpe?g|gif|webp|avif|svg)(?:[?#]|$)/i.test(url) },
          (activate) => <button type="button" className={styles.link} onClick={activate}>{body}</button>);
      }
      return body;
    }
  }
}

function inlineLabel(nodes: readonly SafeInline[]): string {
  return nodes.map((node) => {
    if (node.type === 'text' || node.type === 'inlineCode') return node.value;
    if (node.type === 'image') return node.alt;
    if (node.type === 'break') return ' ';
    return inlineLabel(node.children);
  }).join('');
}
