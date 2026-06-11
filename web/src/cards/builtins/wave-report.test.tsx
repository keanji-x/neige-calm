import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { KernelCard } from '../../api/wire';
import {
  WaveReportEntry,
  waveReportPayloadSchema,
  type WaveReportCardData,
} from './wave-report';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'report_1',
    wave_id: 'wave_1',
    kind: 'wave-report',
    sort: -1,
    payload: {
      schemaVersion: 1,
      summary: 'one-line summary',
      body: '# Goal\n\nrefactor the dispatcher\n',
    },
    deletable: false,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

describe('WaveReportEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('claims kind=wave-report payloads', () => {
    const out = WaveReportEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'wave-report',
      id: 'report_1',
      summary: 'one-line summary',
      body: '# Goal\n\nrefactor the dispatcher\n',
      updatedAt: 2000,
    });
  });

  it('returns null for other kinds', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ kind: 'codex', payload: {} }),
    );
    expect(out).toBeNull();
  });

  it('returns null for invalid payloads', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ payload: { schemaVersion: 1, summary: 'hi' } }),
    );
    expect(out).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });

  it('emits unsupportedVersion and keeps updatedAt for future schema versions', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 99, summary: 'future', body: 'x' },
      }),
    );
    expect(out).toMatchObject({
      type: 'wave-report',
      id: 'report_1',
      updatedAt: 2000,
      unsupportedVersion: 99,
    });
    expect(warnSpy).toHaveBeenCalled();
  });

  it('accepts payloads with missing schemaVersion as v1', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ payload: { summary: 'legacy', body: '# G\n' } }),
    );
    expect(out).toMatchObject({
      type: 'wave-report',
      summary: 'legacy',
      body: '# G\n',
    });
  });
});

describe('waveReportPayloadSchema', () => {
  it('parses the v1 wave-report payload shape', () => {
    expect(
      waveReportPayloadSchema.parse({
        schemaVersion: 1,
        summary: 'summary',
        body: '# Report\n',
      }),
    ).toMatchObject({
      summary: 'summary',
      body: '# Report\n',
    });
  });

  it('rejects missing body or non-string fields', () => {
    expect(() => waveReportPayloadSchema.parse({ summary: 'missing body' }))
      .toThrow();
    expect(() => waveReportPayloadSchema.parse({ summary: 123, body: '# Body' }))
      .toThrow();
  });
});

describe('WaveReportEntry.Component', () => {
  it('is headless', () => {
    const card: WaveReportCardData = {
      type: 'wave-report',
      id: 'report_1',
      summary: 'summary',
      body: '# Report\n',
      updatedAt: 2000,
    };
    expect(WaveReportEntry.Component({ card })).toBeNull();
  });
});
