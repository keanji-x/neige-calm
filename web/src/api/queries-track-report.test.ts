import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from './calm';
import { useTrackReportQuery, trackReportQueryOptions } from './queries';

const { mockUseQuery } = vi.hoisted(() => ({
  mockUseQuery: vi.fn((options: unknown) => options),
}));

vi.mock('@tanstack/react-query', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tanstack/react-query')>()),
  useQuery: mockUseQuery,
}));

vi.mock('./calm', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./calm')>()),
  getTrackReport: vi.fn(),
}));

describe('trackReportQueryOptions', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('rejects malformed task verdicts at the HTTP query boundary', async () => {
    vi.mocked(api.getTrackReport).mockResolvedValue({
      docRev: 1,
      summary: '',
      body: '',
      blocks: [],
      taskDiagnostics: [{
        blockId: 'b_task',
        key: 'task',
        diagnostics: [],
        schedulable: 'yes',
      }],
    } as unknown as api.TrackReportRead);

    await expect(trackReportQueryOptions('track_1').queryFn()).rejects.toThrow();
  });

  // Issue #1147 ① — the reason text has to SURVIVE this boundary, not just
  // exist on the server. `taskBlockVerdictSchema.parse` runs in Zod's
  // default strip mode, so a field missing from the schema is deleted here
  // even though the server sent it and `generated.ts` types it.
  it('preserves statusDetail across the HTTP + Zod boundary', async () => {
    const statusDetail =
      'spawn-failed: track w_1 cwd /home/kenji is not a git repository: '
      + 'fatal: not a git repository';
    vi.mocked(api.getTrackReport).mockResolvedValue({
      docRev: 1,
      summary: '',
      body: '',
      blocks: [],
      taskDiagnostics: [{
        blockId: 'b_task',
        key: 'nogit',
        diagnostics: [],
        schedulable: false,
        status: 'failed',
        statusDetail,
      }],
    } as unknown as api.TrackReportRead);

    const report = await trackReportQueryOptions('track_1').queryFn();

    expect(report.taskDiagnostics[0].statusDetail).toBe(statusDetail);
  });

  it('leaves statusDetail absent when the server omits it', async () => {
    vi.mocked(api.getTrackReport).mockResolvedValue({
      docRev: 1,
      summary: '',
      body: '',
      blocks: [],
      taskDiagnostics: [{
        blockId: 'b_task', key: 'ok', diagnostics: [], schedulable: true,
        status: 'pending',
      }],
    } as unknown as api.TrackReportRead);

    const report = await trackReportQueryOptions('track_1').queryFn();

    expect(report.taskDiagnostics[0].statusDetail).toBeUndefined();
  });

  it.each([1, 0])(
    'the production hook rejects a malformed verdict at index %i',
    async (malformedIndex) => {
      const valid = {
        blockId: 'b_valid', key: 'valid', diagnostics: [], schedulable: true,
        status: 'pending', gateResult: null, workerCardId: null,
      };
      const malformed = {
        blockId: 'b_bad', key: 'bad', diagnostics: [], schedulable: 'yes',
      };
      const taskDiagnostics = malformedIndex === 0
        ? [malformed, valid]
        : [valid, malformed];
      vi.mocked(api.getTrackReport).mockResolvedValue({
        docRev: 1, summary: '', body: '', blocks: [], taskDiagnostics,
      } as unknown as api.TrackReportRead);

      useTrackReportQuery('track_1');
      const options = mockUseQuery.mock.calls.at(-1)?.[0] as {
        queryFn: () => Promise<api.TrackReportRead>;
      };
      await expect(options.queryFn()).rejects.toThrow();
    },
  );
});
