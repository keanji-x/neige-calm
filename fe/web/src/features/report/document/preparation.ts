import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy, sanitizeAstPolicy,
  type MarkdownParseResult,
} from '../../../../../core/markdown/public.ts';
import {
  deriveReportOutline, readTrackReport, trackReportCardIn,
  type ReportBlock, type ReportOutlineItem, type TrackReport,
} from '../../../../../core/domain/report.ts';
import { reportDocumentReferenceRepository, type ReferenceRepository } from '../../../../../core/domain/report-references.ts';
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

/** Report-local work sharing. Core continues to own parsing, excerpts, references and heading policy. */
export function prepareReportDocument(report: TrackReport | null): PreparedReport {
  if (report === null) return { kind: 'empty', report, outline: [], referenceRepository: null };
  const parsed = new Map<string, MarkdownParseResult>();
  const readMarkdown: typeof parse = source => {
    let result = parsed.get(source);
    if (result === undefined) { result = parse(source); parsed.set(source, result); }
    return result;
  };
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

/** Per-router derived data only. Weak keys do not extend query-card reachability; the signed-in scope owns this instance. */
export function createReportPreparationCache() {
  const prepared = new WeakMap<CardWire, PreparedReport>();
  const empty = prepareReportDocument(null);
  return {
    read(cards: readonly CardWire[]): PreparedReport {
      const card = trackReportCardIn(cards);
      if (card === undefined) return empty;
      let result = prepared.get(card);
      if (result === undefined) {
        result = prepareReportDocument(readTrackReport([card]));
        prepared.set(card, result);
      }
      return result;
    },
  };
}
