import { z } from 'zod';
import type { CardEntry } from '../registry';
import {
  payloadSchemaVersion,
  SPEC_PAYLOAD_SCHEMA_VERSION,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    spec: SpecCardData;
  }
}

export interface SpecCardData {
  type: 'spec';
  id?: string;
  goal?: string;
  iconBg?: string;
  iconFg?: string;
  unsupportedVersion?: number;
}

export const specPayloadSchema = z.object({
  spec_harness: z.literal(true),
  schemaVersion: z.number().int().optional(),
  codex_source: z.string().optional(),
  prompt: z.string().optional(),
  icon_bg: z.string().optional(),
  icon_fg: z.string().optional(),
});

export function isSpecHarnessPayload(
  payload: unknown,
): payload is Record<string, unknown> {
  return (
    payload !== null &&
    typeof payload === 'object' &&
    (payload as Record<string, unknown>).spec_harness === true
  );
}

export const SpecEntry: CardEntry<SpecCardData, never> = {
  type: 'spec',
  Component: () => null,
  defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
  refreshBacking: 'none',
  title: () => 'Spec',
  accessibleName: (card) =>
    card.goal?.trim() ? `Spec agent: ${card.goal}` : 'Spec agent',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (k) => {
    if (k.kind !== 'codex') return null;
    const candidate = k.payload ?? {};
    if (!isSpecHarnessPayload(candidate)) return null;
    const version = payloadSchemaVersion(candidate);
    if (version > SPEC_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] spec payload schemaVersion=${version} unsupported (frontend supports ${SPEC_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'spec',
        id: k.id,
        unsupportedVersion: version,
      };
    }
    const parsed = specPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] spec payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'spec',
      id: k.id,
      goal: parsed.data.prompt,
      iconBg: parsed.data.icon_bg,
      iconFg: parsed.data.icon_fg,
    };
  },
};
