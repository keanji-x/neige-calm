import { useMemo, type ReactNode } from 'react';
import { prepareProse, preparePreviewProse, type PreparedProse, type PreviewReferenceContext } from './preparation.ts';
import type { SafeBlock, SafeInline } from '../../../../../core/markdown/public.ts';
import { parseReportLink, type ReportLinkTarget } from '../../../../../core/domain/report.ts';
import { parseReportFileLink, reportFilePathRelativeToRoot, type ReportFileLinkTarget } from '../../../../../core/domain/report-file.ts';
import { parseReportSourceLink, type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { reportTextReferences, qualifiedReportFileReference, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
import { isImagePath, useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { ReadOnlyCode } from '../../../ui/code/public.tsx';
import { ReportLinkPreview, ReportSourceLinkPreview, externalPreviewUrl, type ReportLinkPreviewResources, type PreviewDestination } from '../link-preview/public.tsx';
import styles from './document.module.css';

type ProseOptions = Omit<ReportTextContext, 'referenceRepository'> & Readonly<{
  referenceRepository?: ReferenceRepository | null;
  compact?: boolean;
}>;

/** Standalone reports/previews use the same preparation without retaining navigation capabilities. */
export function ProseBlock({ markdown, blockId, ...options }: ProseOptions & Readonly<{
  markdown: string; blockId: string | null;
}>) {
  const prepared = useMemo(() => prepareProse(markdown, blockId), [markdown, blockId]);
  return <PreparedProseBlock prepared={prepared} {...options} />;
}

function PreviewProse({ markdown, referenceContext, ...options }: ProseOptions & Readonly<{
  markdown: string; referenceContext: PreviewReferenceContext;
}>) {
  const prepared = useMemo(() => preparePreviewProse(markdown, referenceContext), [markdown, referenceContext]);
  return <PreparedProseBlock {...options} prepared={prepared.prose} referenceRepository={prepared.referenceRepository} />;
}

/** The rendered half consumes prepared syntax; admission and live callbacks remain current. */
export function PreparedProseBlock({
  prepared, onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview,
  referenceRepository = null, compact = false, destinationMode = 'interactive',
}: ProseOptions & Readonly<{ prepared: PreparedProse }>) {
  const { result } = prepared;
  if (result.status === 'failed') return <pre className={styles.raw}>{prepared.source}</pre>;
  const { ast, headingIds } = result;
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
      destinationMode={destinationMode}
    />
  ))}</>;
  return compact ? <div className={styles.field}>{rendered}</div> : rendered;
}

export type ReportTextContext = Readonly<{
  destinationMode?: 'interactive' | 'inert';
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
    case 'image': {
      const label = node.alt || 'Image';
      const body = <span className={styles.imageAlt}>{label}</span>;
      if (context.destinationMode === 'inert') return body;
      const target = parseReportFileLink(node.destination);
      const path = target !== null && context.fileRoot !== undefined
        ? reportFilePathRelativeToRoot(context.fileRoot, target, context.fileBasePath) : null;
      if (path !== null && isImagePath(path) && context.linkPreview !== undefined) {
        return <WorkspaceImage key={`${context.linkPreview.trackId}:${path}`} path={path} label={label}
          files={context.linkPreview.files} context={context} />;
      }
      if (externalPreviewUrl(node.destination) !== null) {
        return context.inLink ? body : <Destination destination={node.destination} label={label} body={body} image context={context} />;
      }
      return <UnavailableImage label={label} />;
    }
    case 'link': {
      const label = inlineLabel(node.children);
      const body = <Inlines nodes={node.children} {...context} inLink />;
      if (context.inLink) return body;
      return <Destination destination={node.destination} label={label} body={body} context={context} />;
    }
  }
}

function UnavailableImage({ label }: { label: string }) {
  return <span className={styles.imageAlt}>{label} — Image unavailable</span>;
}

function WorkspaceImage({ path, label, files, context }: {
  path: string; label: string; files: WorkspaceFilePort; context: BlockContext;
}) {
  const resource = useReportFileResource(path, files);
  if (resource.kind === 'error') return <UnavailableImage label={label} />;
  if (resource.kind !== 'image') return <span className={styles.imageAlt}>{label}</span>;
  const image = <img className={styles.image} src={resource.url} alt={label} loading="lazy"
    onLoad={resource.onLoad} onError={resource.onError} />;
  if (context.inLink) return image;
  if (context.onOpenFileLink !== undefined) {
    return <button type="button" className={styles.imageButton} title={path}
      onClick={() => context.onOpenFileLink?.({ path })}>{image}</button>;
  }
  return <Destination destination={path} label={label} body={image} image context={{ ...context, fileBasePath: '' }} />;
}

function Destination({ destination, label, body, image = false, context }: {
  destination: string; label: string; body: ReactNode; image?: boolean; context: ReportTextContext;
}) {
  if (context.destinationMode === 'inert') return body;
  const { onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath, linkPreview } = context;
  const renderMarkdown = (destination: PreviewDestination, text: string, basePath?: string) => <PreviewProse
    markdown={text} {...context} referenceContext={destination.kind === 'reference' ? linkPreview?.report ?? null : 'self'}
    fileBasePath={basePath ?? fileBasePath} />;
  const wrap = (destination: PreviewDestination, trigger: (activate: () => void, dismiss: () => void) => ReactNode, onOpen?: () => void) => (
    <ReportLinkPreview destination={destination} resources={linkPreview} label={label}
      trigger={trigger} onOpen={onOpen} renderMarkdown={(text, basePath) => renderMarkdown(destination, text, basePath)} />
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
