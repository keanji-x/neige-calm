import { expect, it } from 'vitest';

import { readTrackReport } from './report.js';

it('reads persisted layout configuration without replacing report content or identity', () => {
  const payload = { version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{
    kind: 'table', title: 'My journal', span: 1, data: { rows: [] },
    columns: [{ key: 'reason', label: 'Reason', format: 'text', digits: 0 }],
  }] };
  const report = readTrackReport([{ id: 'saved-report', track_id: 'new-instance', kind: 'track-report',
    title: null, sort: 0, deletable: false, created_at: 1, updated_at: 2,
    payload: { schemaVersion: 3, docRev: 42, summary: 'My edited dashboard', body: '# My edited dashboard',
      blocks: [{ id: 'saved-layout', kind: 'layout', rev: 7, payload }] },
  }]);
  expect(report?.summary).toBe('My edited dashboard');
  expect(report?.blocks).toEqual([{ id: 'saved-layout', kind: 'layout', payload }]);
});
