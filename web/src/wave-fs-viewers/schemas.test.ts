import { describe, expect, expectTypeOf, it } from 'vitest';
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

// #1209 PR-2 test #14, leg 3 of 3 (design §3.4) — the FS-snapshot reader.
//
// This reader's input is `wave.json` files ALREADY WRITTEN TO DISK by kernels
// that spelled the template fields `workflow_id` / `workflow_input`. Both new
// fields carry `.default(null)`, so a rename with no compatibility read makes
// every historical snapshot hydrate as `template_id: null` — silently. That is
// the fail-open the normalize step exists to stop.
//
// Deliberately NOT sharing a helper with the two `api/schemas.ts` readers:
// three independent copies mean "the third reader was never wired up" is a red
// test rather than a green one.
describe('#1209 wave.json snapshots written before the template rename', () => {
  const legacySnapshot = {
    id: 'w1',
    area_id: 'c1',
    title: 't',
    sort: 0,
    archived_at: null,
    pinned_at: null,
    lifecycle: 'draft',
    cwd: '/tmp/w1',
    workflow_id: 'small-change',
    plugin_scope: null,
    purpose: null,
    workflow_input: { issue: 1209 },
    terminal_at: null,
    created_at: 1,
    updated_at: 2,
  };

  it('recovers `template_id` from a legacy `workflow_id` key', () => {
    const parsed = waveFsWaveSchema.parse(legacySnapshot);
    expect(parsed.template_id).toBe('small-change');
  });

  it('recovers `template_input` from a legacy `workflow_input` key', () => {
    const parsed = waveFsWaveSchema.parse(legacySnapshot);
    expect(parsed.template_input).toEqual({ issue: 1209 });
  });

  it('drops the legacy keys rather than tripping `.strict()`', () => {
    // The schema is `.strict()`, so a normalize that copied without deleting
    // would turn every legacy snapshot into a hard parse failure instead.
    const parsed = waveFsWaveSchema.parse(legacySnapshot);
    expect(parsed).not.toHaveProperty('workflow_id');
    expect(parsed).not.toHaveProperty('workflow_input');
  });

  it('does not let a legacy key overwrite a present new key', () => {
    const parsed = waveFsWaveSchema.parse({
      ...legacySnapshot,
      template_id: 'investigation',
      template_input: { issue: 1 },
    });
    expect(parsed.template_id).toBe('investigation');
    expect(parsed.template_input).toEqual({ issue: 1 });
  });

  it('still hydrates a snapshot that carries neither spelling', () => {
    const { workflow_id: _id, workflow_input: _input, ...bare } = legacySnapshot;
    const parsed = waveFsWaveSchema.parse(bare);
    expect(parsed.template_id).toBeNull();
    expect(parsed.template_input).toBeNull();
  });
});
