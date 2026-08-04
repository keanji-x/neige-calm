import { describe, expect, it, vi } from 'vitest';
import * as api from './calm';
import { waveReportQueryOptions } from './queries';

vi.mock('./calm', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./calm')>()),
  getWaveReport: vi.fn(),
}));

describe('waveReportQueryOptions', () => {
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
});
