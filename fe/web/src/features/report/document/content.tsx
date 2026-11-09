import { useMemo, type ReactNode } from 'react';
import { prepareProse, preparePreviewProse, type PreparedProse, type PreviewReferenceContext } from './preparation.ts';
import { parseReportLink, type ReportLinkTarget } from '../../../../../core/domain/report.ts';
import { parseReportFileLink, reportFilePathRelativeToRoot, type ReportFileLinkTarget } from '../../../../../core/domain/report-file.ts';
import { parseReportSourceLink, type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { reportTextReferences, qualifiedReportFileReference, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
import { isImagePath, useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { MarkdownBlocks } from '../../../ui/markdown/public.tsx';
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
  const context: ReportTextContext = { onOpenLink, onOpenFileLink, onOpenSourceLink, fileRoot, fileBasePath,
    linkPreview, referenceRepository, destinationMode };
  const rendered = <MarkdownBlocks blocks={ast.children} presentation={{
    classes: { paragraph: styles.p, list: styles.list, item: styles.item, check: styles.check,
      quote: styles.quote, inlineCode: styles.inlineCode, strong: styles.strong,
      tableWrap: styles.tableWrap, table: styles.table, cell: styles.cell, rule: styles.rule },
    heading: (depth, offset, children) => depth === 1
      ? <h2 className={styles.h1} id={headingIds.get(offset)}><span className={styles.headingText}>{children}</span></h2>
      : <h3 className={styles.h2} id={headingIds.get(offset)}>{children}</h3>,
    text: (value, inLink) => inLink ? value : <PlainReportText text={value} context={context} />,
    inlineCode: (value, inLink, body) => inLink || !qualifiedReportFileReference(value) ? body
      : <Destination destination={value} label={value} body={body} context={context} />,
    link: (destination, label, body) => <Destination destination={destination} label={label} body={body} context={context} />,
    image: (destination, alt, inLink) => <ReportImage destination={destination} alt={alt} context={{ ...context, inLink }} />,
  }} />;
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
}>;

/** Table scalars stay literal; only recognized references acquire navigation. */
export function PlainReportText({ text, context }: { text: string; context: ReportTextContext }) {
  return <>{reportTextReferences(text, context.referenceRepository).map((part, index) =>
    part.destination === null ? part.text : <Destination key={index} destination={part.destination} label={part.text} body={part.text} context={context} />
  )}</>;
}

function ReportImage({ destination, alt, context }: { destination: string; alt: string; context: BlockContext }) {
  const label = alt || 'Image';
  const body = <span className={styles.imageAlt}>{label}</span>;
  if (context.destinationMode === 'inert') return body;
  const target = parseReportFileLink(destination);
  const path = target !== null && context.fileRoot !== undefined
    ? reportFilePathRelativeToRoot(context.fileRoot, target, context.fileBasePath) : null;
  if (path !== null && isImagePath(path) && context.linkPreview !== undefined) {
    return <WorkspaceImage key={`${context.linkPreview.trackId}:${path}`} path={path} label={label}
      files={context.linkPreview.files} context={context} />;
  }
  if (externalPreviewUrl(destination) !== null) {
    return context.inLink ? body : <Destination destination={destination} label={label} body={body} image context={context} />;
  }
  return <UnavailableImage label={label} />;
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

/** Narrow link capability for app composition; all admission remains in Destination. */
export function ReportTextLink({ href, children, label, ...context }: ReportTextContext & Readonly<{
  href: string; children: ReactNode; label: string;
}>) {
  return <Destination destination={href} label={label} body={children} context={context} />;
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
