import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from './calm';
import { useWaveReportQuery, waveReportQueryOptions } from './queries';

const { mockUseQuery } = vi.hoisted(() => ({
  mockUseQuery: vi.fn((options: unknown) => options),
}));

vi.mock('@tanstack/react-query', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tanstack/react-query')>()),
  useQuery: mockUseQuery,
}));

vi.mock('./calm', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./calm')>()),
  getWaveReport: vi.fn(),
}));

describe('waveReportQueryOptions', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('rejects malformed task verdicts at the HTTP query boundary', async () => {
    vi.mocked(api.getWaveReport).mockResolvedValue({
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
    } as unknown as api.WaveReportRead);

    await expect(waveReportQueryOptions('wave_1').queryFn()).rejects.toThrow();
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
      vi.mocked(api.getWaveReport).mockResolvedValue({
        docRev: 1, summary: '', body: '', blocks: [], taskDiagnostics,
      } as unknown as api.WaveReportRead);

      useWaveReportQuery('wave_1');
      const options = mockUseQuery.mock.calls.at(-1)?.[0] as {
        queryFn: () => Promise<api.WaveReportRead>;
      };
      await expect(options.queryFn()).rejects.toThrow();
    },
  );
});
