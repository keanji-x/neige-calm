// Reading a wave's report out of its cards.
//
// The report is not a thing this frontend invents: `WaveReportPayload` is a
// Tier-A persisted payload in the kernel (`crates/calm-types/src/wave_report.rs`,
// mirrored into `core/api/generated/wire.ts`), carried in the `payload` column
// of the wave's one `wave-report` card. Its `body` is Markdown source, and the
// kernel is explicit that it "does not interpret the structure" — deriving
// sections by splitting at H1 is the *frontend's* job, which is why
// `core/markdown` already ships `REPORT_MAX_DEPTH = 2`.
//
// Everything here is fail-soft. A card payload is `unknown` on the wire and a
// wave whose report has never been written carries `{}`; neither is an error,
// both are "no report yet".

import { z } from 'zod';

import type { CardWire } from './wave.js';

/** The card kind the kernel reserves for the report. One per wave, undeletable. */
export const WAVE_REPORT_CARD_KIND = 'wave-report';

/**
 * A deliberately *narrow* read of `WaveReportPayload`.
 *
 * `summary` and `body` are the two fields this surface renders. `schemaVersion`,
 * `docRev` and `blocks` are the persistence layer's business — parsing them here
 * would make a v4 payload unreadable to a viewer that does not care what changed.
 * `.catchall` is implicit in zod's default object behaviour: unknown keys pass.
 */
export const waveReportPayloadSchema = z.object({
  summary: z.string().default(''),
  body: z.string().default(''),
});

export type WaveReport = Readonly<{ summary: string; body: string }>;

/**
 * The wave's report, or `null` when it has none.
 *
 * "None" covers three cases that are the same to a reader: no report card, a
 * payload that does not parse, and a payload whose body is blank. A wave that
 * has been created but never worked on is in the third case, which is the
 * common one — so it must not look like a failure.
 */
export function readWaveReport(cards: readonly CardWire[]): WaveReport | null {
  const card = cards.find((candidate) => candidate.kind === WAVE_REPORT_CARD_KIND);
  if (card === undefined) return null;
  const parsed = waveReportPayloadSchema.safeParse(card.payload);
  if (!parsed.success) return null;
  const body = parsed.data.body.trim();
  if (body === '') return null;
  return { summary: parsed.data.summary.trim(), body };
}
