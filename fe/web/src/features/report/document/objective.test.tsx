import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { readTrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

it('renders an objective from the report card as read-only data', () => {
  const report = readTrackReport([{
    id: 'report', track_id: 'track', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { blocks: [{ id: 'objective-1', kind: 'objective', payload: {
      key: 'speed', title: 'Make it fast', mode: 'converge',
      checks: [{ id: 'tests', role: 'goal', config: { kind: 'command', params: { command: 'make test' } } }],
      constraints: [], budget: { turns: 10, tokens: 100000, wall_secs: 3600 }, compact: 'every_settle',
    } }] },
  }]);
  const { container } = render(<ReportDocument report={report} empty={null} />);
  expect(screen.getByRole('table', { name: 'Make it fast' })).toBeTruthy();
  for (const text of ['speed', 'Budget turns', '10', 'Budget tokens', '100000', 'Budget wall time (seconds)', '3600']) {
    expect(screen.getByText(text)).toBeTruthy();
  }
  expect(container.textContent).not.toContain('unsupported');
  expect(container.querySelector('input, textarea, select, [contenteditable="true"]')).toBeNull();
});


it('keeps malformed objectives local to their unsupported note', () => {
  const report = readTrackReport([{
    id: 'report', track_id: 'track', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { blocks: [
      { id: 'bad-objective', kind: 'objective', payload: { title: 'Missing required fields' } },
      { id: 'prose', kind: 'prose', payload: { markdown: 'Still readable.' } },
    ] },
  }]);
  render(<ReportDocument report={report} empty={null} />);
  expect(screen.getByRole('note').textContent).toBe('unsupported block kind objective');
  expect(screen.getByText('Still readable.')).toBeTruthy();
});
