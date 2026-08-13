import { describe, expect, it } from 'vitest';

import type { CardWire } from './wave.js';
import { readWaveReport, WAVE_REPORT_CARD_KIND } from './report.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'c1', wave_id: 'w1', kind: WAVE_REPORT_CARD_KIND, title: null, sort: 0,
    payload: {}, deletable: false, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

describe('readWaveReport', () => {
  it('reads summary and body out of the wave-report card', () => {
    const cards = [
      card({ id: 'other', kind: 'codex', payload: { body: 'not this one' } }),
      card({ payload: { schemaVersion: 3, docRev: 7, summary: 'One line', body: '# Goal\n\nDo the thing.' } }),
    ];
    expect(readWaveReport(cards)).toEqual({ summary: 'One line', body: '# Goal\n\nDo the thing.' });
  });

  it('keeps a report whose summary is written while its body is blank', () => {
    expect(readWaveReport([card({ payload: { summary: 'Agent finished the migration.', body: '  ' } })]))
      .toEqual({ summary: 'Agent finished the migration.', body: '' });
  });

  // A payload from a newer schema must stay readable: this surface renders two
  // fields and has no business rejecting a document because the persistence
  // layer grew a third.
  it('ignores fields it does not render, including ones it has never seen', () => {
    const cards = [card({ payload: { schemaVersion: 9, docRev: 1, summary: 's', body: 'b', blocks: [], future: {} } })];
    expect(readWaveReport(cards)?.body).toBe('b');
  });

  it.each([
    ['no report card at all', [card({ kind: 'codex' })]],
    ['a payload that is not an object', [card({ payload: 'nope' })]],
    ['the untouched payload a fresh wave carries', [card({ payload: {} })]],
    ['a body that is only whitespace', [card({ payload: { body: '   \n  ' } })]],
  ])('reads null for %s', (_label, cards) => {
    expect(readWaveReport(cards as readonly CardWire[])).toBeNull();
  });

  // The three null cases above are one state to a reader — "nothing written
  // yet" — and a wave that was created and never worked on is the common one.
  // It must not be distinguishable from a parse failure at the call site,
  // because the call site would then have to render an error for it.
  it('does not distinguish a fresh wave from an unreadable payload', () => {
    expect(readWaveReport([card({ payload: {} })]))
      .toEqual(readWaveReport([card({ payload: 42 })]));
  });
});
