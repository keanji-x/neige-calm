// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import { readTrackReport } from '../../../../../core/domain/report.ts';
import { ReportTableBlock } from '../table/public.tsx';

afterEach(cleanup);

it('does not reinterpret a table overlay as a different native view', () => {
  render(<ReportTableBlock payload={{ source: 'neige://plugin/operations/health' }} resolveLive={() => ({
    version: 1, view: 'overview', asOf: null, notices: [], charts: [],
    metrics: [{ label: 'Backlog', value: '123', detail: '', tone: 'neutral' }],
  })} />);
  expect(screen.queryByText('123')).toBeNull();
  expect(screen.getByText(/cannot read as a table/)).toBeTruthy();
});

it('recognizes an explicit view.live declaration without a table wrapper', () => {
  const payload = { source: 'neige://plugin/operations/health', version: 1, view: 'overview' };
  const report = readTrackReport([{
    id: 'report', kind: 'track-report', track_id: 't', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { blocks: [{ id: 'b-health', kind: 'view.live', payload }] },
  }]);
  expect(report?.blocks?.[0]).toEqual({ id: 'b-health', kind: 'view.live', payload });
});
