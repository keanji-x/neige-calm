import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import type { KernelCard } from '../../api/wire';
import { CodexEntry } from './codex';
import {
  isPlannerHarnessPayload,
  PlannerEntry,
  plannerPayloadSchema,
} from './planner';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'card_planner_1',
    track_id: 'track_1',
    kind: 'codex',
    sort: 0,
    payload: {
      schemaVersion: 1,
      planner_harness: true,
      prompt: 'Ship the planner UI',
      icon_bg: '#123456',
      icon_fg: '#ffffff',
    },
    deletable: false,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

describe('PlannerEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('maps codex planner-harness payloads into planner cards', () => {
    const out = PlannerEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'planner',
      id: 'card_planner_1',
      goal: 'Ship the planner UI',
      iconBg: '#123456',
      iconFg: '#ffffff',
    });
  });

  it('returns null for non-harness codex cards and non-codex cards', () => {
    expect(
      PlannerEntry.fromKernel!(
        makeKernelCard({ payload: { schemaVersion: 1, terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
    expect(
      PlannerEntry.fromKernel!(
        makeKernelCard({ kind: 'terminal', payload: { terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
  });

  it('rejects malformed planner harness payloads', () => {
    expect(
      PlannerEntry.fromKernel!(
        makeKernelCard({
          payload: { schemaVersion: 1, planner_harness: true, prompt: 123 },
        }),
      ),
    ).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });

  it('emits unsupportedVersion for future schema versions', () => {
    const out = PlannerEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 99, planner_harness: true, prompt: 'new' },
      }),
    );
    expect(out).toMatchObject({
      type: 'planner',
      id: 'card_planner_1',
      unsupportedVersion: 99,
    });
    expect(warnSpy).toHaveBeenCalled();
  });
});

describe('CodexEntry.fromKernel', () => {
  it('does not claim codex planner-harness cards', () => {
    expect(CodexEntry.fromKernel!(makeKernelCard())).toBeNull();
  });

  it('still claims regular codex cards', () => {
    const out = CodexEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 1, terminal_id: 'term_1', cwd: '/tmp' },
      }),
    );
    expect(out).toMatchObject({
      type: 'codex',
      id: 'card_planner_1',
      terminalId: 'term_1',
      cwd: '/tmp',
    });
  });
});

describe('isPlannerHarnessPayload', () => {
  it('identifies planner harness payloads by discriminator only', () => {
    expect(isPlannerHarnessPayload({ planner_harness: true })).toBe(true);
    expect(isPlannerHarnessPayload({ planner_harness: false })).toBe(false);
    expect(isPlannerHarnessPayload({})).toBe(false);
    expect(isPlannerHarnessPayload(null)).toBe(false);
  });
});

describe('plannerPayloadSchema', () => {
  it('parses the v1 planner harness payload shape', () => {
    expect(
      plannerPayloadSchema.parse({
        schemaVersion: 1,
        planner_harness: true,
        codex_source: 'planner',
        prompt: 'Ship it',
        icon_bg: '#000000',
        icon_fg: '#ffffff',
      }),
    ).toMatchObject({
      planner_harness: true,
      prompt: 'Ship it',
    });
  });

  it('rejects payloads without the planner harness discriminator', () => {
    expect(() => plannerPayloadSchema.parse({ prompt: 'missing discriminator' }))
      .toThrow();
  });
});
