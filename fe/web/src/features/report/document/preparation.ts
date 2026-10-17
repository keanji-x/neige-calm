import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy, sanitizeAstPolicy,
  type MarkdownParseResult,
} from '../../../../../core/markdown/public.ts';
import {
  deriveReportOutline, readTrackReport, trackReportCardIn,
  type ReportBlock, type ReportOutlineItem, type TrackReport,
} from '../../../../../core/domain/report.ts';
import { reportDocumentReferenceRepository, reportReferenceRepository, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';

export type PreparedProse = Readonly<{
  source: string;
  result: Extract<MarkdownParseResult, { status: 'failed' }> | Readonly<{
    status: 'ready';
    ast: ReturnType<typeof sanitizeAstPolicy>;
    headingIds: ReadonlyMap<number, string>;
  }>;
}>;

export type PreparedReportBlock =
  | Readonly<{ block: Extract<ReportBlock, { kind: 'prose' }>; prose: PreparedProse }>
  | Readonly<{ block: Exclude<ReportBlock, { kind: 'prose' }>; prose: null }>;

type PreparedMetadata = Readonly<{
  outline: readonly ReportOutlineItem[];
  referenceRepository: ReferenceRepository | null;
}>;

export type PreparedReport = PreparedMetadata & (
  | Readonly<{ kind: 'empty'; report: null }>
  | Readonly<{ kind: 'legacy'; report: TrackReport; prose: PreparedProse; outlineOnly: boolean }>
  | Readonly<{ kind: 'blocks'; report: TrackReport; blocks: readonly PreparedReportBlock[]; outlineOnly: boolean }>
);

/** One syntax preparation; admission and navigation remain in the current renderer context. */
export function prepareProse(source: string, blockId: string | null, readMarkdown: typeof parse = parse): PreparedProse {
  const parsed = readMarkdown(source);
  if (parsed.status === 'failed') return { source, result: parsed };
  const headingIds = blockId === null ? new Map<number, string>()
    : new Map(extractOutline([{ context: { blockId }, ast: parsed.value }], {
      maxDepth: REPORT_MAX_DEPTH, headingId: reportHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label', referenceText: 'visible', traversal: 'recursive',
    }).map(heading => [heading.position.start.offset ?? -1, heading.id]));
  return { source, result: { status: 'ready', ast: sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' }), headingIds } };
}

function headingsOnly(prose: PreparedProse): boolean {
  return prose.result.status === 'ready' && prose.result.ast.children.every(block => block.type === 'heading');
}

/** Local to one preparation, including failed parses; the returned model never retains this reader. */
function sharedMarkdownReader(): typeof parse {
  const parsed = new Map<string, MarkdownParseResult>();
  return source => {
    let result = parsed.get(source);
    if (result === undefined) { result = parse(source); parsed.set(source, result); }
    return result;
  };
}

/** A file uses `self`; a referenced block uses its whole report (`null` when that context is absent). */
export type PreviewReferenceContext = 'self' | TrackReport | null;
export function preparePreviewProse(source: string, context: PreviewReferenceContext) {
  const readMarkdown = sharedMarkdownReader();
  const referenceRepository = context === 'self'
    ? reportReferenceRepository([source], readMarkdown)
    : reportDocumentReferenceRepository(context, readMarkdown);
  return { prose: prepareProse(source, null, readMarkdown), referenceRepository };
}

/** Report-local work sharing. Core continues to own parsing, excerpts, references and heading policy. */
export function prepareReportDocument(report: TrackReport | null): PreparedReport {
  if (report === null) return { kind: 'empty', report, outline: [], referenceRepository: null };
  const readMarkdown = sharedMarkdownReader();
  const referenceRepository = reportDocumentReferenceRepository(report, readMarkdown);
  const outline = deriveReportOutline(report.blocks, readMarkdown);
  if (report.blocks === null) {
    const prose = prepareProse(report.body || report.summary, null, readMarkdown);
    return { kind: 'legacy', report, outline, referenceRepository, prose, outlineOnly: headingsOnly(prose) };
  }
  const blocks: PreparedReportBlock[] = report.blocks.map(block => block.kind === 'prose'
    ? { block, prose: prepareProse(block.payload.markdown, block.id, readMarkdown) }
    : { block, prose: null });
  return { kind: 'blocks', report, outline, referenceRepository, blocks,
    outlineOnly: blocks.every(slot => slot.prose !== null && headingsOnly(slot.prose)) };
}

export type ReportPreparationPort = (report: TrackReport | null, signal: AbortSignal) => PreparedReport | Promise<PreparedReport>;

/** Cheap canonical input identity. It contains no prepared syntax or live capabilities. */
export type ReportSource = Readonly<{ card: CardWire | null; report: TrackReport | null }>;

/** Per-router derived data only. Weak keys do not extend query-card reachability. */
export function createReportPreparationCache() {
  const sources = new WeakMap<CardWire, ReportSource>();
  const prepared = new WeakMap<ReportSource, PreparedReport>();
  const emptySource: ReportSource = { card: null, report: null };
  const empty = prepareReportDocument(null);
  prepared.set(emptySource, empty);
  const sourceOf = (cards: readonly CardWire[]): ReportSource => {
    const card = trackReportCardIn(cards);
    if (card === undefined) return emptySource;
    let source = sources.get(card);
    if (source === undefined) {
      source = { card, report: readTrackReport([card]) };
      sources.set(card, source);
      if (source.report === null) prepared.set(source, empty);
    }
    return source;
  };
  return {
    source: sourceOf,
    peek: (source: ReportSource): PreparedReport | undefined => prepared.get(source),
    read(cards: readonly CardWire[]): PreparedReport {
      const source = sourceOf(cards);
      let result = prepared.get(source);
      if (result === undefined) {
        result = prepareReportDocument(source.report);
        prepared.set(source, result);
      }
      return result;
    },
    async prepare(source: ReportSource, execute: ReportPreparationPort, signal: AbortSignal): Promise<PreparedReport> {
      signal.throwIfAborted();
      const cached = prepared.get(source);
      if (cached !== undefined) return cached;
      const result = await execute(source.report, signal);
      signal.throwIfAborted();
      prepared.set(source, result);
      return result;
    },
  };
}

export type ReportPreparationCache = ReturnType<typeof createReportPreparationCache>;
