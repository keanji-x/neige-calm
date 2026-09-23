import { describe, expect, it } from 'vitest';
import { deriveReportPresentation, readTrackReport } from '../../../../core/domain/report.ts';
import type { CardWire } from '../../../../core/domain/track.ts';

function report(blocks: unknown, body = '') {
  const card: CardWire = { id: 'report', track_id: 'track', kind: 'track-report', title: 'portfolioDemo',
    sort: 1, deletable: false, created_at: 1, updated_at: 1, payload: { blocks, body } };
  return readTrackReport([card]);
}

describe('declared report presentation', () => {
  it('keeps a declared native view wide even when its payload cannot yet be read', () => {
    expect(deriveReportPresentation(report([{ id: 'view', kind: 'view', payload: { version: 99 } }])))
      .toBe('dashboard');
  });

  it.each(['prose', 'view.live', 'app', 'chart.series', 'portfolioDemo', 'unsupported'])(
    'does not treat %s, a title, or body text as a native declaration', kind => {
      expect(deriveReportPresentation(report([{ id: 'view', kind, payload: {} }], 'view portfolioDemo dashboard')))
        .toBe('document');
    },
  );

  it('keeps absent, empty, and legacy reports as documents', () => {
    for (const value of [null, report([]), report(null, 'dashboard'), report([{ kind: 'view', payload: {} }])]) {
      expect(deriveReportPresentation(value)).toBe('document');
    }
  });
});
