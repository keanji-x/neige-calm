import { describe, expect, it } from 'vitest';
import { excludeReportCards } from './excludeReportCards';
import type { WaveCardSlot } from '../types';
import type { TerminalCardData } from './builtins/terminal';
import type { SpecCardData } from './builtins/spec';
import type { WaveReportCardData } from './builtins/wave-report';

function terminal(id: string): WaveCardSlot {
  const card: TerminalCardData = {
    type: 'terminal',
    id,
    title: 'terminal',
    lines: [],
  };
  return { kind: 'card', card };
}

function report(id = 'report_1'): WaveCardSlot {
  const card: WaveReportCardData = {
    type: 'wave-report',
    id,
    summary: '',
    body: '# Report',
  };
  return { kind: 'card', card, sort: -1, deletable: false };
}

function spec(id = 'spec_1'): WaveCardSlot {
  const card: SpecCardData = {
    type: 'spec',
    id,
    goal: 'Plan the work',
  };
  return { kind: 'card', card, sort: 0, deletable: false };
}

describe('excludeReportCards', () => {
  it('returns an empty array for an empty card list', () => {
    expect(excludeReportCards([])).toEqual([]);
  });

  it('leaves worker cards untouched', () => {
    const cards = [terminal('term_1'), terminal('term_2')];
    expect(excludeReportCards(cards)).toEqual([
      { slot: cards[0], originalIndex: 0 },
      { slot: cards[1], originalIndex: 1 },
    ]);
  });

  it('excludes a wave-report card', () => {
    expect(excludeReportCards([report()])).toEqual([]);
  });

  it('excludes a spec card', () => {
    expect(excludeReportCards([spec()])).toEqual([]);
  });

  it('preserves original indexes through a mixed card list', () => {
    const cards = [
      terminal('term_1'),
      report(),
      terminal('term_2'),
      spec(),
      terminal('term_3'),
    ];

    const out = excludeReportCards(cards);

    expect(out.map((x) => x.slot)).toEqual([cards[0], cards[2], cards[4]]);
    expect(out.map((x) => x.originalIndex)).toEqual([0, 2, 4]);
  });

  it('excludes unknown wave-report kernel cards defensively', () => {
    const cards: WaveCardSlot[] = [
      terminal('term_1'),
      { kind: 'unknown', id: 'unknown_report', kernelKind: 'wave-report' },
    ];

    expect(excludeReportCards(cards)).toEqual([
      { slot: cards[0], originalIndex: 0 },
    ]);
  });
});
