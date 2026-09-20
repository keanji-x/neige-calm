// The track-report card: headless, one per track; its contents are rendered by the report document runtime.

import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    trackReport: TrackReportCard;
  }
}

export type TrackReportCard = Readonly<{ type: 'track-report'; id: string }>;

export const TRACK_REPORT_CARD_ENTRY = Object.freeze({
  type: 'track-report',
  component: () => null,
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Report',
  accessibleName: () => 'Track report',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): TrackReportCard | null => (
    card.kind === 'track-report' ? Object.freeze({ type: 'track-report', id: card.id } as const) : null
  ),
}) satisfies CardEntry<TrackReportCard>;
