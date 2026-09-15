/*
 * A captured source behind a `neige://source/<id>[#q<n>]` citation (#1669).
 *
 * The kernel stores what a plugin call returned as an immutable body on the
 * track (`report_sources`), mints `src_` + 8 hex for it, and lets the planner
 * pin `q<n>` anchors — byte-exact substrings of that body. The report cites
 * it with this one scheme; this module is the browser's reading of the link
 * and of the row it names: the parser, the request, and the arithmetic the
 * panel does before it paints (which slice of the body is the quote).
 *
 * It is a **sibling** of `parseReportLink`, not an extension of it. The
 * track-citation scanner feeds task dependencies, frozen contexts and
 * backlinks, none of which may learn about sources (I5); the two schemes stay
 * two functions so that they cannot.
 */

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import type { SourceProvenance, TrackSourceDetail } from '../api/generated/wire.js';

export const SOURCE_LINK_PREFIX = 'neige://source/';

/** `src_` + 8 lowercase hex — what the kernel mints, and nothing looser. */
const SOURCE_ID_PATTERN = /^src_[0-9a-f]{8}$/;

/** `q` + a positive decimal without leading zeros — the kernel's anchor ids. */
const QUOTE_ID_PATTERN = /^q[1-9][0-9]*$/;

export const SOURCE_PROVENANCES = Object.freeze(['full_text', 'summary', 'web_page', 'manual'] as const);

/**
 * What a source citation resolves to.
 *
 * A destination under the prefix is always a citation — clickable, so that a
 * dangling or misspelt one is still *visible* as a citation that failed
 * rather than silently degrading to prose — but only a well-formed one names
 * a source. A malformed id or a malformed anchor leaves `sourceId` null: the
 * kernel treats those links as unresolved as a whole (its receipt warnings
 * name them), and the page says the same thing, "来源缺失", with the
 * destination beside it so the author can see what was written.
 */
export type ReportSourceLinkTarget = Readonly<{
  /** The link as written — the panel prints it when nothing resolves. */
  destination: string;
  /** The source to fetch, or `null` when the link cannot name one. */
  sourceId: string | null;
  /** The anchor to highlight, or `null` when there is none (or the link is unresolvable). */
  quoteId: string | null;
}>;

/**
 * Parse a Markdown destination as a source citation.
 *
 * `null` when the destination is not under `neige://source/` at all — every
 * other scheme is somebody else's (`parseReportLink`, `parseReportFileLink`)
 * or nobody's. Under the prefix, always a target; see the type for what a
 * malformed one carries.
 */
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

/* ── The row ─────────────────────────────────────────────────────────── */

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

/**
 * `GET /api/tracks/{id}/sources/{source_id}` — the row with its body. The
 * decoder is strict about the fields the panel paints and lenient about the
 * rest, the way every other domain decoder is.
 */
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

/**
 * The read as the panel sees it. `missing` is data, not an error: a dangling
 * citation is a state the design admits (the kernel does not refuse the
 * write), so the panel shows it as one rather than retrying a 404.
 */
export type SourceResolution =
  | Readonly<{ status: 'loading' }>
  | Readonly<{ status: 'missing' }>
  | Readonly<{ status: 'error'; message: string }>
  | Readonly<{ status: 'ok'; source: TrackSourceDetail }>;

/* ── The highlight ───────────────────────────────────────────────────── */

/**
 * The body split around the quote: the text before it, the quote, the text
 * after — or `null` when the anchor cannot be placed.
 *
 * The slice is found with `indexOf`, **first occurrence**, on the quote's
 * `text`; the row's `start`/`end` are UTF-8 byte offsets meant for the kernel
 * and are not used here (I2 promises `text` is a byte-exact substring, so
 * the first occurrence is what the kernel anchored). `null` covers both a
 * `quoteId` the row does not carry and a text that is not in the body — the
 * second should be impossible under I2, and the panel says "anchor missed"
 * rather than trusting it.
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
