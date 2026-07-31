import { describe, expect, it } from 'vitest';
import type { ReportBlock } from '../cards/builtins/wave-report';
import { deriveOutline } from './report-outline';

function prose(id: string, markdown: string): ReportBlock {
  return { id, kind: 'prose', rev: 1, payload: { markdown } };
}

describe('deriveOutline', () => {
  it('extracts H2 headings in document order and ignores other heading levels', () => {
    expect(
      deriveOutline([
        prose('b_1', '# Report\n\n## First *section*'),
        prose('b_2', 'Second section\n--------------'),
        prose('b_3', '### Detail\n\n> ## Third `section`'),
      ]),
    ).toMatchObject([
      { blockId: 'b_1', label: 'First section', number: 1 },
      { blockId: 'b_2', label: 'Second section', number: 2 },
      { blockId: 'b_3', label: 'Third section', number: 3 },
    ]);
  });

  it('does not treat fenced or indented code as H2 headings', () => {
    expect(
      deriveOutline([
        prose(
          'b_1',
          '```md\n## fenced\n```\n\n    ## indented\n\n## Real heading',
        ),
      ]),
    ).toMatchObject([{ label: 'Real heading', number: 1 }]);
  });

  it('uses the child label fallback chain and nests blocks under the preceding H2', () => {
    const blocks = [
      prose('b_head', '## Market'),
      {
        id: 'b_chart',
        kind: 'chart.candles',
        rev: 1,
        payload: {
          symbol: '0700.HK',
          src: '/ignored',
          candles: [[1, 1, 1, 1, 1], [2, 2, 2, 2, 2]],
        },
      },
      {
        id: 'b_table',
        kind: 'table',
        rev: 1,
        payload: {
          columns: [{ key: 'k', label: 'Key' }],
          rows: [],
          caption: 'Valuation',
          title: 'Ignored title',
        },
      },
      {
        id: 'b_app',
        kind: 'app',
        rev: 1,
        payload: { src: '/apps/model', title: 'Ignored app title' },
      },
    ] as ReportBlock[];

    expect(deriveOutline(blocks)[0]?.children).toEqual([
      { blockId: 'b_chart', kind: 'chart.candles', label: '0700.HK' },
      { blockId: 'b_table', kind: 'table', label: 'Valuation' },
      { blockId: 'b_app', kind: 'app', label: '/apps/model' },
    ]);
  });

  it('handles unknown kinds with the same fallback chain and falls back to kind', () => {
    const blocks = [
      prose('b_head', '## Extras'),
      {
        id: 'b_unknown_title',
        kind: 'holo.gram',
        rev: 1,
        payload: { title: 'Projection' },
      },
      {
        id: 'b_unknown_kind',
        kind: 'mystery',
        rev: 1,
        payload: {},
      },
    ] as ReportBlock[];

    expect(deriveOutline(blocks)[0]?.children).toEqual([
      { blockId: 'b_unknown_title', kind: 'holo.gram', label: 'Projection' },
      { blockId: 'b_unknown_kind', kind: 'mystery', label: 'mystery' },
    ]);
  });

  it('returns no navigation for both no-block downgrade paths', () => {
    expect(deriveOutline(undefined)).toEqual([]);
    expect(deriveOutline([])).toEqual([]);
  });
});
