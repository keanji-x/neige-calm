import { describe, expect, it } from 'vitest';

import type { CardWire } from './wave.js';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, groupBacklinks, parseReportLink,
  readWaveReport, WAVE_REPORT_CARD_KIND, type WaveBacklink,
} from './report.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'c1', wave_id: 'w1', kind: WAVE_REPORT_CARD_KIND, title: null, sort: 0,
    payload: {}, deletable: false, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

function prose(id: string, markdown: string) {
  return { id, kind: 'prose', rev: 1, payload: { markdown } };
}

describe('readWaveReport', () => {
  it('reads summary and body out of the wave-report card', () => {
    const cards = [
      card({ id: 'other', kind: 'codex', payload: { body: 'not this one' } }),
      card({ payload: { schemaVersion: 3, docRev: 7, summary: 'One line', body: '# Goal\n\nDo the thing.' } }),
    ];
    expect(readWaveReport(cards)).toEqual({ summary: 'One line', body: '# Goal\n\nDo the thing.', blocks: null });
  });

  it('keeps a report whose summary is written while its body is blank', () => {
    expect(readWaveReport([card({ payload: { summary: 'Agent finished the migration.', body: '  ' } })]))
      .toEqual({ summary: 'Agent finished the migration.', body: '', blocks: null });
  });

  // A payload from a newer schema must stay readable: this surface renders a
  // known set of block kinds and has no business rejecting a document because
  // the persistence layer grew a field.
  it('ignores fields it does not render, including ones it has never seen', () => {
    const cards = [card({ payload: { schemaVersion: 9, docRev: 1, summary: 's', body: 'b', future: {} } })];
    expect(readWaveReport(cards)?.body).toBe('b');
  });

  it.each([
    ['no report card at all', [card({ kind: 'codex' })]],
    ['a payload that is not an object', [card({ payload: 'nope' })]],
    ['the untouched payload a fresh wave carries', [card({ payload: {} })]],
    ['a body that is only whitespace', [card({ payload: { body: '   \n  ' } })]],
    ['a blank body next to an empty blocks array', [card({ payload: { body: '', blocks: [] } })]],
  ])('reads null for %s', (_label, cards) => {
    expect(readWaveReport(cards as readonly CardWire[])).toBeNull();
  });

  // The null cases above are one state to a reader — "nothing written yet" —
  // and a wave that was created and never worked on is the common one. It must
  // not be distinguishable from a parse failure at the call site, because the
  // call site would then have to render an error for it.
  it('does not distinguish a fresh wave from an unreadable payload', () => {
    expect(readWaveReport([card({ payload: {} })]))
      .toEqual(readWaveReport([card({ payload: 42 })]));
  });

  it('reads the typed blocks, which are what carries each block id', () => {
    const report = readWaveReport([card({
      payload: {
        body: '# Goal',
        blocks: [
          prose('b-1', '# Goal'),
          { id: 'b-2', kind: 'table', rev: 3, payload: { columns: [{ key: 'k', label: 'K' }], rows: [{ k: 'v' }] } },
        ],
      },
    })]);
    expect(report?.blocks?.map((block) => [block.id, block.kind]))
      .toEqual([['b-1', 'prose'], ['b-2', 'table']]);
  });

  it('drops one wire-invalid block while keeping the other blocks', () => {
    const report = readWaveReport([card({
      payload: {
        body: '# Goal',
        blocks: [prose('b-1', '# Goal'), { id: '', kind: 'prose', payload: {} }, prose('b-2', 'Still here.')],
      },
    })]);
    expect(report?.blocks?.map((block) => block.id)).toEqual(['b-1', 'b-2']);
  });

  it('falls back to body when blocks is not an array', () => {
    expect(readWaveReport([card({ payload: { body: '# Goal', blocks: 'nope' } })]))
      .toEqual({ summary: '', body: '# Goal', blocks: null });
  });

  it('keeps a task live when an absent optional field is explicit null', () => {
    const report = readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [{
          id: 'b-1', kind: 'task', rev: 1,
          payload: {
            key: 'task-1', kind: 'codex', goal: 'Fix it', ready: true,
            declared_by: 'spec', spawn: null,
          },
        }],
      },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('task');
  });

  it('accepts a 2048-code-point string even when emoji use two UTF-16 code units', () => {
    const src = `/${'😀'.repeat(2047)}`;
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('app');
  });

  // The entrance fee of the block model: a report is written by an agent, so it
  // will contain something this build does not know about. That must cost one
  // block, never the page.
  it.each([
    ['a kind this build has never seen', { id: 'b-1', kind: 'chart.sankey', rev: 1, payload: { nodes: [] } }],
    ['a known kind whose payload does not fit', { id: 'b-1', kind: 'table', rev: 1, payload: { columns: [] } }],
    ['a known kind whose payload is not an object', { id: 'b-1', kind: 'prose', rev: 1, payload: 7 }],
  ])('degrades %s to one unsupported block, keeping the others', (_label, bad) => {
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [bad, prose('b-2', 'Still here.')] },
    })]);
    expect(report?.blocks?.[0]).toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: bad.kind });
    expect(report?.blocks?.[1]?.kind).toBe('prose');
  });

  // `src` is loaded into an iframe, so its validator is the one place a
  // rejected payload is a security outcome and not a rendering one.
  it.each([
    ['a protocol-relative path', '//evil.example/x'],
    ['a backslash the browser would normalize to a slash', '/\\evil.example/x'],
    ['an absolute URL', 'https://evil.example/x'],
    ['a control character', '/apps/a\0b'],
  ])('refuses an app block whose src is %s', (_label, src) => {
    const report = readWaveReport([card({
      payload: { body: 'x', blocks: [{ id: 'b-1', kind: 'app', rev: 1, payload: { src } }] },
    })]);
    expect(report?.blocks?.[0]?.kind).toBe('unsupported');
  });
});

describe('deriveReportTasks', () => {
  function tasksOf(blocks: unknown[]) {
    return deriveReportTasks(readWaveReport([card({ payload: { body: 'x', blocks } })])?.blocks ?? null);
  }

  function task(id: string, payload: Record<string, unknown>) {
    return { id, kind: 'task', rev: 1, payload };
  }

  const live = (key: string, ready: boolean) =>
    ({ key, kind: 'codex', declared_by: 'spec', ready, goal: 'g' });

  it('lists every task in document order and nothing else', () => {
    expect(tasksOf([
      prose('b-1', '# One\n'),
      task('b-2', live('alpha', true)),
      { id: 'b-3', kind: 'table', rev: 1, payload: { caption: 'c', columns: [], rows: [] } },
      task('b-4', live('beta', false)),
    ])).toEqual([
      { blockId: 'b-2', key: 'alpha', state: 'ready' },
      { blockId: 'b-4', key: 'beta', state: 'not-ready' },
    ]);
  });

  /* Same discriminant as the block renderer, and the same trap: a *live* task
     may carry an explicit `tombstone: null`, so the presence of that key proves
     nothing. Reading it as withdrawn would strike through a task that is
     running. */
  it('reads a task carrying an explicit null tombstone as live, not withdrawn', () => {
    expect(tasksOf([task('b-1', { ...live('alpha', true), tombstone: null })]))
      .toEqual([{ blockId: 'b-1', key: 'alpha', state: 'ready' }]);
  });

  /* Kept for the same reason the block keeps it: the task existed, other
     reports may cite its block id, and a panel that dropped it would disagree
     with the document it is derived from. */
  it('keeps a withdrawn task', () => {
    expect(tasksOf([task('b-1', {
      key: 'gone', declared_by: 'spec', tombstoned_by: 'user', tombstone: { reason: 'r' },
    })])).toEqual([{ blockId: 'b-1', key: 'gone', state: 'withdrawn' }]);
  });

  /* A block whose payload this build cannot read degrades to `unsupported`,
     which has no key and no state — inventing a row for it would put a task in
     the panel that the document does not draw. */
  it('drops a task block whose payload does not parse', () => {
    expect(tasksOf([task('b-1', { key: 'broken' })])).toEqual([]);
  });

  it('has no rows for a report with no blocks', () => {
    expect(deriveReportTasks(null)).toEqual([]);
  });
});

describe('deriveReportOutline', () => {
  it('numbers sections continuously across prose blocks, never restarting per block', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [prose('b-1', '# One\n\ntext\n\n## Two\n'), prose('b-2', '# Three\n')],
      },
    })])?.blocks ?? null);
    expect(outline.map((item) => [item.number, item.label, item.blockId])).toEqual([
      [1, 'One', 'b-1-h1'],
      [2, 'Two', 'b-1-h2'],
      [3, 'Three', 'b-2-h1'],
    ]);
  });

  it('hangs a non-prose block under the section above it, as evidence rather than a section', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [
          prose('b-1', '# Valuation\n'),
          { id: 'b-2', kind: 'chart.candles', rev: 1, payload: { symbol: '600519', candles: [[0, 1, 2, 0, 1], [1, 1, 2, 0, 1]] } },
        ],
      },
    })])?.blocks ?? null);
    expect(outline).toHaveLength(1);
    expect(outline[0]?.children).toEqual([{ blockId: 'b-2', label: '600519' }]);
  });

  /* Tasks are no longer drawn in the flow — `features/report/document` lifts
     them into the collapsed `Reference` appendix — so hanging them under the
     section they used to follow would point the outline at a place they are not.
     On a real wave that was eight rows of machinery in a map of the argument.

     The assertion pairs a task with a *kept* non-prose block, because "the
     outline is empty" would also pass a rule that dropped everything. */
  it('leaves task blocks out: they are not in the document flow any more', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [
          prose('b-1', '# Valuation\n'),
          { id: 'b-2', kind: 'task', rev: 1, payload: { key: 'alpha', kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' } },
          { id: 'b-3', kind: 'table', rev: 1, payload: { caption: 'Comparables', columns: [{ key: 'k', label: 'K' }], rows: [] } },
        ],
      },
    })])?.blocks ?? null);
    expect(outline).toHaveLength(1);
    expect(outline[0]?.children).toEqual([{ blockId: 'b-3', label: 'Comparables' }]);
  });

  it('promotes a leading non-prose block to an unnumbered top-level item', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: {
        body: 'x',
        blocks: [
          { id: 'b-1', kind: 'table', rev: 1, payload: { columns: [{ key: 'k', label: 'K' }], rows: [], caption: 'Comparables' } },
          prose('b-2', '# After\n'),
        ],
      },
    })])?.blocks ?? null);
    expect(outline.map((item) => [item.number, item.label])).toEqual([
      [null, 'Comparables'],
      [1, 'After'],
    ]);
  });

  // Deeper than H2 is not a section: `REPORT_MAX_DEPTH` is the kernel's own
  // splitting rule, and an outline that listed H3s would promise a level the
  // document does not have.
  it('ignores headings deeper than the report max depth', () => {
    const outline = deriveReportOutline(readWaveReport([card({
      payload: { body: 'x', blocks: [prose('b-1', '# One\n\n### Deep\n')] },
    })])?.blocks ?? null);
    expect(outline.map((item) => item.label)).toEqual(['One']);
  });

  it('is empty for a v1 report, which has no block ids to anchor to', () => {
    expect(deriveReportOutline(null)).toEqual([]);
  });
});

describe('backlinks', () => {
  const backlink = (overrides: Partial<WaveBacklink> = {}): WaveBacklink => ({
    src_wave_id: 'w-2', src_wave_title: 'Cash flow model', src_block_id: 'b-9',
    dst_block_id: 'b-1', label: 'valuation', quote: null, updated_at: 0, ...overrides,
  });

  it('groups by source wave and names a self-reference as such', () => {
    const groups = groupBacklinks(
      [backlink(), backlink({ src_block_id: 'b-10' }), backlink({ src_wave_id: 'w-1' })],
      'w-1',
    );
    expect(groups.map((group) => [group.waveId, group.title, group.entries.length])).toEqual([
      ['w-2', 'Cash flow model', 2],
      ['w-1', 'This wave (self-reference)', 1],
    ]);
  });

  it('counts backlinks per target block and ignores whole-wave citations', () => {
    const counts = backlinkCountsByBlock([
      backlink(), backlink({ src_block_id: 'b-11' }), backlink({ dst_block_id: null }),
    ]);
    expect([...counts]).toEqual([['b-1', 2]]);
  });
});

describe('parseReportLink', () => {
  it('resolves a wave link with a block fragment', () => {
    expect(parseReportLink('neige://wave/w-2#b-1')).toEqual({ waveId: 'w-2', blockId: 'b-1' });
  });

  it('resolves a wave link without a fragment', () => {
    expect(parseReportLink('neige://wave/w-2')).toEqual({ waveId: 'w-2', blockId: null });
  });

  it('keeps a malformed percent escape as a usable raw wave target', () => {
    expect(() => parseReportLink('neige://wave/%E0%A4%A')).not.toThrow();
    expect(parseReportLink('neige://wave/%E0%A4%A'))
      .toEqual({ waveId: '%E0%A4%A', blockId: null });
  });

  // Landing at the top of the right report beats a dead link, so a malformed
  // fragment costs the fragment and not the destination.
  it('drops a malformed fragment but keeps the wave', () => {
    expect(parseReportLink('neige://wave/w-2#../../etc/passwd'))
      .toEqual({ waveId: 'w-2', blockId: null });
  });

  it.each([
    ['a plain http url', 'https://example.com'],
    ['another neige noun', 'neige://cove/c-1'],
    ['a javascript url', 'javascript:alert(1)'],
  ])('reads null for %s, which the renderer then shows as plain text', (_label, url) => {
    expect(parseReportLink(url)).toBeNull();
  });
});
