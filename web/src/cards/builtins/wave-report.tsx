import { z } from 'zod';
import type { CardEntry } from '../registry';
import {
  WAVE_REPORT_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    'wave-report': WaveReportCardData;
  }
}

export interface WaveReportCardData {
  type: 'wave-report';
  id?: string;
  summary: string;
  body: string;
  updatedAt?: number;
  unsupportedVersion?: number;
}

/** Strict zod schema for the wire payload. `schemaVersion` may be
 *  absent (treated as v1). */
export const waveReportPayloadSchema = z.object({
  schemaVersion: z.number().int().optional(),
  summary: z.string(),
  body: z.string(),
});

export const WaveReportEntry: CardEntry<WaveReportCardData> = {
  type: 'wave-report',
  Component: () => null,
  defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
  claim: { mode: 'exact', kind: 'wave-report' },
  title: () => 'Report',
  accessibleName: (card) =>
    card.summary.trim().length > 0 ? `Report: ${card.summary}` : 'Report',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (k) => {
    if (k.kind !== 'wave-report') return null;
    const candidate = k.payload ?? {};
    const version = payloadSchemaVersion(candidate);
    if (version > WAVE_REPORT_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload schemaVersion=${version} unsupported (frontend supports ${WAVE_REPORT_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'wave-report',
        id: k.id,
        summary: '',
        body: '',
        updatedAt: k.updated_at,
        unsupportedVersion: version,
      };
    }
    const parsed = waveReportPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'wave-report',
      id: k.id,
      summary: parsed.data.summary,
      body: parsed.data.body,
      updatedAt: k.updated_at,
    };
  },
};
