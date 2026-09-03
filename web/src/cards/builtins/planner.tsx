import { z } from 'zod';
import type { CardEntry } from '../registry';
import {
  payloadSchemaVersion,
  PLANNER_PAYLOAD_SCHEMA_VERSION,
} from './schemaVersions';

declare module '../../types' {
  interface TrackCardDataMap {
    planner: PlannerCardData;
  }
}

export interface PlannerCardData {
  type: 'planner';
  title?: string | null;
  id?: string;
  goal?: string;
  iconBg?: string;
  iconFg?: string;
  unsupportedVersion?: number;
}

export const plannerPayloadSchema = z.object({
  planner_harness: z.literal(true),
  schemaVersion: z.number().int().optional(),
  codex_source: z.string().optional(),
  prompt: z.string().optional(),
  icon_bg: z.string().optional(),
  icon_fg: z.string().optional(),
});

export function isPlannerHarnessPayload(
  payload: unknown,
): payload is Record<string, unknown> {
  return (
    payload !== null &&
    typeof payload === 'object' &&
    (payload as Record<string, unknown>).planner_harness === true
  );
}

export const PlannerEntry: CardEntry<PlannerCardData, never> = {
  type: 'planner',
  Component: () => null,
  defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
  refreshBacking: 'none',
  title: (card) => card.title || 'Planner',
  accessibleName: (card) =>
    card.goal?.trim() ? `Planner agent: ${card.goal}` : 'Planner agent',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (k) => {
    if (k.kind !== 'codex') return null;
    const candidate = k.payload ?? {};
    if (!isPlannerHarnessPayload(candidate)) return null;
    const version = payloadSchemaVersion(candidate);
    if (version > PLANNER_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] planner payload schemaVersion=${version} unsupported (frontend supports ${PLANNER_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'planner',
        id: k.id,
        title: k.title,
        unsupportedVersion: version,
      };
    }
    const parsed = plannerPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] planner payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'planner',
      id: k.id,
      goal: parsed.data.prompt,
      iconBg: parsed.data.icon_bg,
      iconFg: parsed.data.icon_fg,
    };
  },
};
