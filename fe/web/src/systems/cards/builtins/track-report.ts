// The track-report card (`INV-CARD-201`).
//
// Every track has exactly one, and its contents are already rendered by the
// report document runtime (`features/report/document`). The card exists so the
// track knows the report is there; it has no surface of its own and must never
// occupy a slot in the CARDS list or the grid.

import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    trackReport: TrackReportCard;
  }
}

export type TrackReportCard = Readonly<{ type: 'track-report'; id: string }>;

/**
 * `INV-CARD-201` — headless, kernel-minted only, no add-panel entry point.
 *
 * The kernel kind is unambiguous here, but the entry still resolves through
 * `fromKernel` rather than an exact `claim` so that every builtin goes through
 * one resolution path.
 */
export const TRACK_REPORT_CARD_ENTRY = Object.freeze({
  type: 'track-report',
  component: () => null,
  // The declaration `partitionTrackCards` reads. Dropping it puts the report
  // back into the CARDS list as an empty panel beside the document that
  // already renders it.
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Report',
  accessibleName: () => 'Track report',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): TrackReportCard | null => (
    card.kind === 'track-report' ? Object.freeze({ type: 'track-report', id: card.id } as const) : null
  ),
}) satisfies CardEntry<TrackReportCard>;
