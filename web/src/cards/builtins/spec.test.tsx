import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import type { KernelCard } from '../../api/wire';
import { CodexEntry } from './codex';
import {
  isSpecHarnessPayload,
  SpecEntry,
  specPayloadSchema,
} from './spec';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'card_spec_1',
    wave_id: 'wave_1',
    kind: 'codex',
    sort: 0,
    payload: {
      schemaVersion: 1,
      spec_harness: true,
      prompt: 'Ship the spec UI',
      icon_bg: '#123456',
      icon_fg: '#ffffff',
    },
    deletable: false,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

describe('SpecEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('maps codex spec-harness payloads into spec cards', () => {
    const out = SpecEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'spec',
      id: 'card_spec_1',
      goal: 'Ship the spec UI',
      iconBg: '#123456',
      iconFg: '#ffffff',
    });
  });

  it('returns null for non-harness codex cards and non-codex cards', () => {
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({ payload: { schemaVersion: 1, terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({ kind: 'terminal', payload: { terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
  });

  it('rejects malformed spec harness payloads', () => {
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({
          payload: { schemaVersion: 1, spec_harness: true, prompt: 123 },
        }),
      ),
    ).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });

  it('emits unsupportedVersion for future schema versions', () => {
    const out = SpecEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 99, spec_harness: true, prompt: 'new' },
      }),
    );
    expect(out).toMatchObject({
      type: 'spec',
      id: 'card_spec_1',
      unsupportedVersion: 99,
    });
    expect(warnSpy).toHaveBeenCalled();
  });
});

describe('CodexEntry.fromKernel', () => {
  it('does not claim codex spec-harness cards', () => {
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
      id: 'card_spec_1',
      terminalId: 'term_1',
      cwd: '/tmp',
    });
  });
});

describe('isSpecHarnessPayload', () => {
  it('identifies spec harness payloads by discriminator only', () => {
    expect(isSpecHarnessPayload({ spec_harness: true })).toBe(true);
    expect(isSpecHarnessPayload({ spec_harness: false })).toBe(false);
    expect(isSpecHarnessPayload({})).toBe(false);
    expect(isSpecHarnessPayload(null)).toBe(false);
  });
});

describe('specPayloadSchema', () => {
  it('parses the v1 spec harness payload shape', () => {
    expect(
      specPayloadSchema.parse({
        schemaVersion: 1,
        spec_harness: true,
        codex_source: 'spec',
        prompt: 'Ship it',
        icon_bg: '#000000',
        icon_fg: '#ffffff',
      }),
    ).toMatchObject({
      spec_harness: true,
      prompt: 'Ship it',
    });
  });

  it('rejects payloads without the spec harness discriminator', () => {
    expect(() => specPayloadSchema.parse({ prompt: 'missing discriminator' }))
      .toThrow();
  });
});
