import { describe, expectTypeOf, it } from 'vitest';
import type { z } from 'zod';
import type {
  AgentProvider,
  CardRuntimeView,
  CardRole,
  WorkerSessionKind,
  WorkerSessionState,
  Wave,
  WaveFsCardMeta,
  WaveFsHookEvent,
  WaveFsRunDetail,
  WaveFsRunEventRef,
  WaveFsRunEvents,
  WaveFsRunIndexEntry,
  WaveFsRunStatus,
  WaveFsRunVerdict,
  WaveFsRunVerdictSummary,
} from '../api/generated-events';
import {
  agentProviderSchema,
  cardRuntimeSchema,
  cardRuntimeViewSchema,
  runtimeKindSchema,
  workerSessionStateSchema,
  waveFsCardMetaSchema,
  waveFsCardRoleSchema,
  waveFsCardsIndexSchema,
  waveFsHookEventsSchema,
  waveFsHookEventSchema,
  waveFsRunDetailSchema,
  waveFsRunEventRefSchema,
  waveFsRunEventsSchema,
  waveFsRunIndexEntrySchema,
  waveFsRunsIndexSchema,
  waveFsRunStatusSchema,
  waveFsRunVerdictSchema,
  waveFsRunVerdictSummarySchema,
  waveFsWaveSchema,
} from './schemas';

describe('wave fs zod to generated type conformance', () => {
  it('pins enum schemas to generated unions', () => {
    expectTypeOf<z.infer<typeof agentProviderSchema>>()
      .toEqualTypeOf<AgentProvider>();
    expectTypeOf<z.infer<typeof workerSessionStateSchema>>()
      .toEqualTypeOf<WorkerSessionState>();
    expectTypeOf<z.infer<typeof runtimeKindSchema>>()
      .toEqualTypeOf<WorkerSessionKind>();
    expectTypeOf<z.infer<typeof waveFsCardRoleSchema>>().toEqualTypeOf<CardRole>();
    expectTypeOf<z.infer<typeof waveFsRunStatusSchema>>()
      .toEqualTypeOf<WaveFsRunStatus>();
  });

  it('pins card and wave schemas to generated shapes', () => {
    expectTypeOf<z.infer<typeof waveFsCardMetaSchema>>()
      .toEqualTypeOf<WaveFsCardMeta>();
    expectTypeOf<z.infer<typeof waveFsCardsIndexSchema>>()
      .toEqualTypeOf<WaveFsCardMeta[]>();
    expectTypeOf<z.infer<typeof waveFsWaveSchema>>().toEqualTypeOf<Wave>();
  });

  it('pins run schemas to generated shapes', () => {
    expectTypeOf<z.infer<typeof waveFsRunVerdictSummarySchema>>()
      .toEqualTypeOf<WaveFsRunVerdictSummary>();
    expectTypeOf<z.infer<typeof waveFsRunVerdictSchema>>()
      .toEqualTypeOf<WaveFsRunVerdict>();
    expectTypeOf<z.infer<typeof waveFsRunEventRefSchema>>()
      .toEqualTypeOf<WaveFsRunEventRef>();
    expectTypeOf<z.infer<typeof waveFsRunEventsSchema>>()
      .toEqualTypeOf<WaveFsRunEvents>();
    expectTypeOf<z.infer<typeof waveFsRunIndexEntrySchema>>()
      .toEqualTypeOf<WaveFsRunIndexEntry>();
    expectTypeOf<z.infer<typeof waveFsRunsIndexSchema>>()
      .toEqualTypeOf<WaveFsRunIndexEntry[]>();
    expectTypeOf<z.infer<typeof waveFsRunDetailSchema>>()
      .toEqualTypeOf<WaveFsRunDetail>();
  });

  it('pins hook-event schema for future event viewers', () => {
    expectTypeOf<z.infer<typeof waveFsHookEventSchema>>()
      .toEqualTypeOf<WaveFsHookEvent>();
    expectTypeOf<z.infer<typeof waveFsHookEventsSchema>>()
      .toEqualTypeOf<WaveFsHookEvent[]>();
  });

  it('pins runtime schemas to generated shapes', () => {
    expectTypeOf<z.infer<typeof cardRuntimeViewSchema>>()
      .toEqualTypeOf<CardRuntimeView>();
    expectTypeOf<z.infer<typeof cardRuntimeSchema>>()
      .toEqualTypeOf<CardRuntimeView | null>();
  });
});
