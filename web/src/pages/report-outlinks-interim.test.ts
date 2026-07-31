import { describe, expect, it } from 'vitest';
import type { ReportBlock } from '../cards/builtins/wave-report';
import { deriveInterimReportOutlinks } from './report-outlinks-interim';

describe('deriveInterimReportOutlinks', () => {
  it('extracts unique Markdown links in first-seen order', () => {
    const blocks = [{
      id: 'b_links',
      kind: 'prose',
      rev: 1,
      payload: {
        markdown:
          '[First](neige://wave/wave_a#b_0001) then ' +
          '[Second](neige://wave/wave_b) and ' +
          '[First again](neige://wave/wave_a#b_0002)',
      },
    }] as ReportBlock[];

    expect(deriveInterimReportOutlinks(blocks)).toEqual([
      { waveId: 'wave_a', blockId: 'b_0001' },
      { waveId: 'wave_b', blockId: undefined },
    ]);
  });

  it('ignores fenced code, inline code, and bare URLs', () => {
    const blocks = [{
      id: 'b_code',
      kind: 'prose',
      rev: 1,
      payload: {
        markdown: [
          'neige://wave/wave_bare',
          '`neige://wave/wave_inline`',
          '```md',
          '[Example](neige://wave/wave_fenced)',
          '```',
          '[Real](neige://wave/wave_real#b_cafe)',
        ].join('\n'),
      },
    }] as ReportBlock[];

    expect(deriveInterimReportOutlinks(blocks)).toEqual([
      { waveId: 'wave_real', blockId: 'b_cafe' },
    ]);
  });
});
