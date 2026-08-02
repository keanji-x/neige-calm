import { describe, expect, it } from 'vitest';
import type { ReportBlock } from '../cards/builtins/wave-report';
import { deriveOutline } from './report-outline';

function prose(id: string, markdown: string): ReportBlock {
  return { id, kind: 'prose', rev: 1, payload: { markdown } };
}

describe('deriveOutline', () => {
  it('extracts pure H1 reports in document order', () => {
    expect(
      deriveOutline([
        prose('b_1', '# Summary\n\n# Findings\n\n# Recommendation'),
      ]),
    ).toMatchObject([
      { blockId: 'b_1-h1', label: 'Summary', number: 1 },
      { blockId: 'b_1-h2', label: 'Findings', number: 2 },
      { blockId: 'b_1-h3', label: 'Recommendation', number: 3 },
    ]);
  });

  it('numbers mixed H1 and H2 headings in document order', () => {
    expect(
      deriveOutline([
        prose('b_1', '# Summary\n\n## Evidence\n\n# Recommendation'),
      ]),
    ).toMatchObject([
      { blockId: 'b_1-h1', label: 'Summary', number: 1 },
      { blockId: 'b_1-h2', label: 'Evidence', number: 2 },
      { blockId: 'b_1-h3', label: 'Recommendation', number: 3 },
    ]);
  });

  it('keeps an H1 after an H2 in source order across prose blocks', () => {
    expect(
      deriveOutline([
        prose('b_1', '## First'),
        prose('b_2', '# Second'),
        prose('b_3', '## Third'),
      ]),
    ).toMatchObject([
      { blockId: 'b_1-h1', label: 'First', number: 1 },
      { blockId: 'b_2-h1', label: 'Second', number: 2 },
      { blockId: 'b_3-h1', label: 'Third', number: 3 },
    ]);
  });

  it('extracts H2 headings in document order and ignores deeper levels', () => {
    expect(
      deriveOutline([
        prose('b_1', '### Report\n\n## First *section*'),
        prose('b_2', 'Second section\n--------------'),
        prose('b_3', '### Detail\n\n> ## Third `section`'),
      ]),
    ).toMatchObject([
      { blockId: 'b_1-h1', label: 'First section', number: 1 },
      { blockId: 'b_2-h1', label: 'Second section', number: 2 },
      { blockId: 'b_3-h1', label: 'Third section', number: 3 },
    ]);
  });

  it('gives every H2 in one prose block its own deterministic target', () => {
    expect(deriveOutline([prose('b_multi', '## First\n\n## Second')]))
      .toMatchObject([
        { blockId: 'b_multi-h1', label: 'First', number: 1 },
        { blockId: 'b_multi-h2', label: 'Second', number: 2 },
      ]);
  });

  it('does not treat fenced or indented code as section headings', () => {
    expect(
      deriveOutline([
        prose(
          'b_1',
          '```md\n## fenced\n```\n\n    ## indented\n\n## Real heading',
        ),
      ]),
    ).toMatchObject([{ label: 'Real heading', number: 1 }]);
  });

  it('uses the child label fallback chain and nests blocks under the preceding section', () => {
    const blocks = [
      prose('b_head', '# Market'),
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

  it('keeps leading non-prose blocks as unnumbered top-level entries', () => {
    const blocks = [
      {
        id: 'b_chart',
        kind: 'chart.candles',
        rev: 1,
        payload: { symbol: '0700.HK', candles: [] },
      },
      prose('b_head', '## Market'),
    ] as ReportBlock[];

    expect(deriveOutline(blocks)).toMatchObject([
      {
        blockId: 'b_chart',
        label: '0700.HK',
        number: null,
        children: [],
      },
      { blockId: 'b_head-h1', label: 'Market', number: 1 },
    ]);
  });

  it('returns no navigation for both no-block downgrade paths', () => {
    expect(deriveOutline(undefined)).toEqual([]);
    expect(deriveOutline([])).toEqual([]);
  });
});
