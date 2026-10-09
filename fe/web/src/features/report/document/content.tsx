import type { ReactNode } from 'react';
import { extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy, sanitizeAstPolicy, type SafeBlock, type SafeInline } from '../../../../../core/markdown/public.ts';
import { parseReportLink, type ReportLinkTarget } from '../../../../../core/domain/report.ts';
import { parseReportFileLink, reportFilePathRelativeToRoot, type ReportFileLinkTarget } from '../../../../../core/domain/report-file.ts';
import { parseReportSourceLink, type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { reportTextReferences, qualifiedReportFileReference, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
import { ReadOnlyCode } from '../../../ui/code/public.tsx';
import { ReportLinkPreview, ReportSourceLinkPreview, externalPreviewUrl, type ReportLinkPreviewResources, type PreviewDestination } from '../link-preview/public.tsx';
import styles from './document.module.css';

/** The rendered-Markdown half of a report block. Exported for the recipe editor. */
export function ProseBlock({
  markdown, blockId, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview, referenceRepository = null, compact = false,
}: {
  markdown: string;
  referenceRepository?: ReferenceRepository | null;
  compact?: boolean;
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
  const rendered = <>{ast.children.map((block, index) => (
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
      referenceRepository={referenceRepository}
    />
  ))}</>;
  return compact ? <div className={styles.field}>{rendered}</div> : rendered;
}

export type ReportTextContext = Readonly<{
  referenceRepository: ReferenceRepository | null;
  onOpenLink?: (target: ReportLinkTarget) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  fileRoot?: string;
  fileBasePath?: string;
  linkPreview?: ReportLinkPreviewResources;
}>;

type BlockContext = ReportTextContext & Readonly<{
  inLink?: boolean;
  headingIds: ReadonlyMap<number, string>;
}>;

/** Table scalars stay literal; only recognized references acquire navigation. */
export function PlainReportText({ text, context }: { text: string; context: ReportTextContext }) {
  return <>{reportTextReferences(text, context.referenceRepository).map((part, index) =>
    part.destination === null ? part.text : <Destination key={index} destination={part.destination} label={part.text} body={part.text} context={context} />
  )}</>;
}

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
  switch (node.type) {
    case 'text':
      if (context.inLink) return node.value;
      return <PlainReportText text={node.value} context={context} />;
    case 'inlineCode': {
      const code = <code className={styles.inlineCode}>{node.value}</code>;
      return context.inLink || !qualifiedReportFileReference(node.value) ? code
        : <Destination destination={node.value} label={node.value} body={code} context={context} />;
    }
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
      return <Destination destination={node.destination} label={label} body={body} image={image} context={context} />;
    }
  }
}

function Destination({ destination, label, body, image = false, context }: {
  destination: string; label: string; body: ReactNode; image?: boolean; context: ReportTextContext;
}) {
  const { onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview } = context;
  const renderMarkdown = (text: string, basePath?: string) => <ProseBlock
    markdown={text} blockId={null} {...context} fileBasePath={basePath ?? fileBasePath} />;
  const wrap = (destination: PreviewDestination, trigger: (activate: () => void, dismiss: () => void) => ReactNode, onOpen?: () => void) => (
    <ReportLinkPreview destination={destination} resources={linkPreview} label={label}
      trigger={trigger} onOpen={onOpen} renderMarkdown={renderMarkdown} />
  );
  const target = image ? null : parseReportLink(destination);
  if (target !== null && (onOpenLink !== undefined || linkPreview !== undefined)) {
    const open = onOpenLink === undefined ? undefined : () => onOpenLink(target);
    return wrap({ kind: 'reference', destination, target },
      (activate, dismiss) => <button type="button" className={styles.link} onClick={open === undefined ? activate : () => { dismiss(); open(); }}>{body}</button>, open);
  }
  const sourceTarget = image ? null : parseReportSourceLink(destination);
  if (sourceTarget !== null) {
    return <ReportSourceLinkPreview target={sourceTarget} label={label} resources={linkPreview}
      onOpen={onOpenSourceLink}>{body}</ReportSourceLinkPreview>;
  }
  const fileTarget = parseReportFileLink(destination);
  const path = fileTarget !== null && fileRoot !== undefined
    ? reportFilePathRelativeToRoot(fileRoot, fileTarget, fileBasePath) : null;
  if (path !== null && (onOpenFileLink !== undefined || linkPreview !== undefined)) {
    const open = onOpenFileLink === undefined ? undefined : () => onOpenFileLink({ path });
    return wrap({ kind: 'file', path },
      (activate, dismiss) => <button type="button" className={styles.link} title={path} onClick={open === undefined ? activate : () => { dismiss(); open(); }}>{body}</button>, open);
  }
  const url = externalPreviewUrl(destination);
  if (url !== null) {
    return wrap({ kind: 'web', url, image: image || /\.(png|jpe?g|gif|webp|avif|svg)(?:[?#]|$)/i.test(url) },
      (activate) => <button type="button" className={styles.link} onClick={activate}>{body}</button>);
  }
  return body;
}

function inlineLabel(nodes: readonly SafeInline[]): string {
  return nodes.map((node) => {
    if (node.type === 'text' || node.type === 'inlineCode') return node.value;
    if (node.type === 'image') return node.alt;
    if (node.type === 'break') return ' ';
    return inlineLabel(node.children);
  }).join('');
}
