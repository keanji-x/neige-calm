/*
 * A captured source behind a `neige://source/<id>[#q<n>]` citation: the parser, the request,
 * and the slice of the body that is the quote. A sibling of `parseReportLink`, not an extension:
 * the track-citation scanner must never learn about sources.
 */

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import type { SourceProvenance, TrackSourceDetail } from '../api/generated/wire.js';
import { parse, sanitizeAstPolicy, type SafeInline } from '../markdown/public.js';

export const SOURCE_LINK_PREFIX = 'neige://source/';

/** `src_` + 8 lowercase hex — what the kernel mints, and nothing looser. */
const SOURCE_ID_PATTERN = /^src_[0-9a-f]{8}$/;

/** `q` + a positive decimal without leading zeros — the kernel's anchor ids. */
const QUOTE_ID_PATTERN = /^q[1-9][0-9]*$/;

export const SOURCE_PROVENANCES = Object.freeze(['full_text', 'summary', 'web_page', 'manual'] as const);

/**
 * A destination under the prefix is always a citation (so a dangling one stays visible), but only a
 * well-formed one names a source.
 */
export type ReportSourceLinkTarget = Readonly<{
  /** The link as written — the panel prints it when nothing resolves. */
  destination: string;
  /** The source to fetch, or `null` when the link cannot name one. */
  sourceId: string | null;
  /** The anchor to highlight, or `null` when there is none (or the link is unresolvable). */
  quoteId: string | null;
}>;

/** `null` when the destination is not under `neige://source/` at all; under the prefix, always a target. */
export function parseReportSourceLink(destination: string): ReportSourceLinkTarget | null {
  if (!destination.startsWith(SOURCE_LINK_PREFIX)) return null;
  const path = destination.slice(SOURCE_LINK_PREFIX.length);
  const hash = path.indexOf('#');
  const sourceId = hash < 0 ? path : path.slice(0, hash);
  const fragment = hash < 0 ? null : path.slice(hash + 1);
  const unresolved: ReportSourceLinkTarget = { destination, sourceId: null, quoteId: null };
  if (!SOURCE_ID_PATTERN.test(sourceId)) return unresolved;
  if (fragment === null) return { destination, sourceId, quoteId: null };
  if (!QUOTE_ID_PATTERN.test(fragment)) return unresolved;
  return { destination, sourceId, quoteId: fragment };
}

export type SourceCitationCell = Readonly<{
  /** The link's label, as plain text (see `parseSourceCitationCell`). */
  label: string;
  target: ReportSourceLinkTarget;
}>;

/**
 * Read a `table` cell as one source citation and nothing else: exactly one paragraph holding
 * exactly one link under `neige://source/…`, decided by the parser so the cell agrees with the
 * prose beside it. `null` for every other cell.
 */
export function parseSourceCitationCell(text: string): SourceCitationCell | null {
  const parsed = parse(text);
  if (parsed.status !== 'ready') return null;
  const blocks = sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' }).children;
  const paragraph = blocks.length === 1 ? blocks[0] : undefined;
  if (paragraph === undefined || paragraph.type !== 'paragraph' || paragraph.children.length !== 1) return null;
  const link = paragraph.children[0];
  if (link === undefined || link.type !== 'link') return null;
  const target = parseReportSourceLink(link.destination);
  return target === null ? null : { label: inlineLabel(link.children), target };
}

function inlineLabel(nodes: readonly SafeInline[]): string {
  return nodes.map((node) => {
    switch (node.type) {
      case 'text':
      case 'inlineCode':
        return node.value;
      case 'image':
        return node.alt;
      case 'break':
        return ' ';
      default:
        return inlineLabel(node.children);
    }
  }).join('');
}

export const sourceQuoteSchema = z.object({
  id: z.string(),
  text: z.string(),
  start: z.number().int().nonnegative(),
  end: z.number().int().nonnegative(),
});

const sourceOriginSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('plugin'),
    plugin_id: z.string(),
    tool: z.string(),
    args_sha256: z.string(),
    args_canon: z.string(),
    content_id: z.string().optional(),
  }),
  z.object({
    kind: z.literal('manual'),
    url: z.string().optional(),
    content_id: z.string().optional(),
  }),
]);

/** `GET /api/tracks/{id}/sources/{source_id}`: strict about the fields the panel paints, lenient about the rest. */
export const trackSourceDetailSchema: z.ZodType<TrackSourceDetail> = z.object({
  source_id: z.string(),
  provenance: z.enum(SOURCE_PROVENANCES),
  origin: sourceOriginSchema,
  title: z.string(),
  published_at: z.string().optional(),
  content_id: z.string().optional(),
  url: z.string().optional(),
  body_bytes: z.number().int().nonnegative(),
  body_sha256: z.string(),
  captured_at: z.string(),
  quotes: z.array(sourceQuoteSchema),
  body: z.string(),
});

export type { SourceProvenance, TrackSourceDetail };

export function trackSourceOperation(trackId: string, sourceId: string): ApiOperation<TrackSourceDetail> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/sources/${encodeURIComponent(sourceId)}`,
    responseSchema: trackSourceDetailSchema,
  };
}

/** `missing` is data, not an error: a dangling citation is a state the design admits. */
export type SourceResolution =
  | Readonly<{ status: 'loading' }>
  | Readonly<{ status: 'missing' }>
  | Readonly<{ status: 'error'; message: string }>
  | Readonly<{ status: 'ok'; source: TrackSourceDetail }>;

/**
 * The body split around the quote, or `null` when the anchor cannot be placed. Found with
 * `indexOf`, first occurrence; the row's byte offsets are for the kernel and not used here.
 */
export type SourceHighlight = Readonly<{ before: string; quote: string; after: string }>;

export function sourceHighlight(source: Pick<TrackSourceDetail, 'body' | 'quotes'>, quoteId: string): SourceHighlight | null {
  const quote = source.quotes.find((entry) => entry.id === quoteId);
  if (quote === undefined || quote.text === '') return null;
  const start = source.body.indexOf(quote.text);
  if (start < 0) return null;
  const end = start + quote.text.length;
  return { before: source.body.slice(0, start), quote: source.body.slice(start, end), after: source.body.slice(end) };
}
