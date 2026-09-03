import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { KernelCard } from '../../api/wire';
import {
  TrackReportEntry,
  trackReportPayloadSchema,
  type TrackReportCardData,
} from './track-report';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'report_1',
    track_id: 'track_1',
    kind: 'track-report',
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

describe('TrackReportEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('claims kind=track-report payloads', () => {
    const out = TrackReportEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'track-report',
      id: 'report_1',
      summary: 'one-line summary',
      body: '# Goal\n\nrefactor the dispatcher\n',
      updatedAt: 2000,
    });
  });

  it('returns null for other kinds', () => {
    const out = TrackReportEntry.fromKernel!(
      makeKernelCard({ kind: 'codex', payload: {} }),
    );
    expect(out).toBeNull();
  });

  it('returns null for invalid payloads', () => {
    const out = TrackReportEntry.fromKernel!(
      makeKernelCard({ payload: { schemaVersion: 1, summary: 'hi' } }),
    );
    expect(out).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });

  it('emits unsupportedVersion and keeps updatedAt for future schema versions', () => {
    const out = TrackReportEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 99, summary: 'future', body: 'x' },
      }),
    );
    expect(out).toMatchObject({
      type: 'track-report',
      id: 'report_1',
      updatedAt: 2000,
      unsupportedVersion: 99,
    });
    expect(warnSpy).toHaveBeenCalled();
  });

  it('accepts payloads with missing schemaVersion as v1', () => {
    const out = TrackReportEntry.fromKernel!(
      makeKernelCard({ payload: { summary: 'legacy', body: '# G\n' } }),
    );
    expect(out).toMatchObject({
      type: 'track-report',
      summary: 'legacy',
      body: '# G\n',
    });
  });
});

describe('trackReportPayloadSchema', () => {
  it('parses the v1 track-report payload shape', () => {
    expect(
      trackReportPayloadSchema.parse({
        schemaVersion: 1,
        summary: 'summary',
        body: '# Report\n',
      }),
    ).toMatchObject({
      summary: 'summary',
      body: '# Report\n',
      docRev: 0,
    });
  });

  it('rejects missing body or non-string fields', () => {
    expect(() => trackReportPayloadSchema.parse({ summary: 'missing body' }))
      .toThrow();
    expect(() => trackReportPayloadSchema.parse({ summary: 123, body: '# Body' }))
      .toThrow();
  });
});

describe('TrackReportEntry.Component', () => {
  it('is headless', () => {
    const card: TrackReportCardData = {
      type: 'track-report',
      id: 'report_1',
      summary: 'summary',
      body: '# Report\n',
      updatedAt: 2000,
      docRev: 0,
    };
    expect(TrackReportEntry.Component({ card })).toBeNull();
  });
});
