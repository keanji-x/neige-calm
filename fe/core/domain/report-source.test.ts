import { describe, expect, it } from 'vitest';

import { parseReportLink } from './report.js';
import { parseReportFileLink } from './report-file.js';
import {
  parseReportSourceLink, parseSourceCitationCell, sourceHighlight, trackSourceDetailSchema, trackSourceOperation,
} from './report-source.js';

describe('parseReportSourceLink', () => {
  it('reads a well-formed citation, with and without an anchor', () => {
    expect(parseReportSourceLink('neige://source/src_2c9e0a1b'))
      .toEqual({ destination: 'neige://source/src_2c9e0a1b', sourceId: 'src_2c9e0a1b', quoteId: null });
    expect(parseReportSourceLink('neige://source/src_2c9e0a1b#q1'))
      .toEqual({ destination: 'neige://source/src_2c9e0a1b#q1', sourceId: 'src_2c9e0a1b', quoteId: 'q1' });
    expect(parseReportSourceLink('neige://source/src_ffffffff#q32'))
      .toEqual({ destination: 'neige://source/src_ffffffff#q32', sourceId: 'src_ffffffff', quoteId: 'q32' });
  });

  /* The kernel's `report_source_links::scan` keeps malformed links and flags
     them so the receipt can warn about them; the page keeps them clickable for
     the same reason — a citation that failed must be visible as one. */
  it('keeps a malformed id clickable but unresolvable, destination intact', () => {
    for (const destination of [
      'neige://source/src_dead',
      'neige://source/src_2C9E0A1B',
      'neige://source/2c9e0a1b',
      'neige://source/',
      'neige://source/src_dead#q1',
      'neige://source/src_2c9e0a1b/extra',
    ]) {
      expect(parseReportSourceLink(destination), destination)
        .toEqual({ destination, sourceId: null, quoteId: null });
    }
  });

  it('treats a malformed anchor as the whole link being unresolvable, like the kernel does', () => {
    for (const destination of [
      'neige://source/src_2c9e0a1b#q0',
      'neige://source/src_2c9e0a1b#q01',
      'neige://source/src_2c9e0a1b#Q1',
      'neige://source/src_2c9e0a1b#b_1f3a',
      'neige://source/src_2c9e0a1b#',
      'neige://source/src_2c9e0a1b#q1#q2',
    ]) {
      expect(parseReportSourceLink(destination), destination)
        .toEqual({ destination, sourceId: null, quoteId: null });
    }
  });

  it('is nobody else\'s scheme: not a track citation, not a file, not http', () => {
    for (const destination of [
      'neige://plugin/dev-neige-market/market.series',
      'https://example.com/src_2c9e0a1b',
      './docs/src_2c9e0a1b.md',
      'NEIGE://source/src_2c9e0a1b',
      '',
    ]) {
      expect(parseReportSourceLink(destination), destination).toBeNull();
    }
    // …and the other two parsers do not claim a source link either (I5).
    expect(parseReportLink('neige://source/src_2c9e0a1b#q1')).toBeNull();
    expect(parseReportFileLink('neige://source/src_2c9e0a1b#q1')).toBeNull();
  });
});

/* #1687 — a table cell is one citation iff the prose's own parser reads it
   as exactly one paragraph holding exactly one link under the source scheme.
   The parser, not a pattern, so the cell and the prose beside it agree. */
describe('parseSourceCitationCell', () => {
  const TARGET = { destination: 'neige://source/src_ddef99cc#q1', sourceId: 'src_ddef99cc', quoteId: 'q1' };

  it('reads a cell that is exactly one source link', () => {
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1)')).toEqual({ label: 'AP', target: TARGET });
    expect(parseSourceCitationCell('[智堡所载UBS摘要](neige://source/src_04d04fc3)')).toEqual({
      label: '智堡所载UBS摘要',
      target: { destination: 'neige://source/src_04d04fc3', sourceId: 'src_04d04fc3', quoteId: null },
    });
  });

  it('follows Markdown on brackets: a stray `[` is text beside a link, balanced ones are one label', () => {
    expect(parseSourceCitationCell('[[AP](neige://source/src_ddef99cc#q1)')).toBeNull();
    expect(parseSourceCitationCell('[AP [Reuters]](neige://source/src_ddef99cc#q1)'))
      .toEqual({ label: 'AP [Reuters]', target: TARGET });
  });

  it('applies the parser\'s own whitespace rules: trailing and light leading space vanish, an indent is code', () => {
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1) ')).toEqual({ label: 'AP', target: TARGET });
    expect(parseSourceCitationCell('  [AP](neige://source/src_ddef99cc#q1)\n')).toEqual({ label: 'AP', target: TARGET });
    expect(parseSourceCitationCell('    [AP](neige://source/src_ddef99cc#q1)')).toBeNull();
  });

  it('is null for a link with prose around it, two links, or two paragraphs', () => {
    expect(parseSourceCitationCell('见 [AP](neige://source/src_ddef99cc#q1) 收盘')).toBeNull();
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1) 收盘')).toBeNull();
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1) [UBS](neige://source/src_04d04fc3#q2)')).toBeNull();
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1)\n\nmore')).toBeNull();
  });

  it('is null for a link under any other scheme, for markup, and for an empty cell', () => {
    expect(parseSourceCitationCell('[x](neige://report/b_1#s1)')).toBeNull();
    expect(parseSourceCitationCell('[x](https://example.com/a)')).toBeNull();
    expect(parseSourceCitationCell('<a href="https://example.com">x</a>')).toBeNull();
    expect(parseSourceCitationCell('')).toBeNull();
    expect(parseSourceCitationCell('28.4')).toBeNull();
  });

  it('drops raw HTML before counting, as the prose does, and keeps a malformed id as a citation', () => {
    expect(parseSourceCitationCell('[AP](neige://source/src_ddef99cc#q1)<b>')).toEqual({ label: 'AP', target: TARGET });
    expect(parseSourceCitationCell('[坏链接](neige://source/src_zz)')).toEqual({
      label: '坏链接', target: { destination: 'neige://source/src_zz', sourceId: null, quoteId: null },
    });
  });

  it('projects the label to plain text', () => {
    expect(parseSourceCitationCell('[**AP** `x` ![alt](i.png)](neige://source/src_ddef99cc#q1)'))
      .toEqual({ label: 'AP x alt', target: TARGET });
  });
});

describe('sourceHighlight', () => {
  const body = '央行表示，9月加息概率接近九成。市场认为，9月加息概率接近九成的说法过于乐观。';

  it('slices the first occurrence of the quote out of the body', () => {
    const quote = '9月加息概率接近九成';
    const highlight = sourceHighlight({ body, quotes: [{ id: 'q1', text: quote, start: 0, end: 0 }] }, 'q1');
    expect(highlight).toEqual({
      before: '央行表示，',
      quote,
      after: '。市场认为，9月加息概率接近九成的说法过于乐观。',
    });
    // The three pieces are the body, byte for byte.
    expect(`${highlight?.before}${highlight?.quote}${highlight?.after}`).toBe(body);
  });

  it('places a quote by its text, not by the kernel\'s byte offsets', () => {
    // Byte offsets into UTF-8 are not code-unit offsets into a JS string; a
    // slice by them would land mid-character on CJK text.
    const highlight = sourceHighlight({ body, quotes: [{ id: 'q1', text: '市场认为', start: 45, end: 57 }] }, 'q1');
    expect(highlight?.quote).toBe('市场认为');
    expect(highlight?.before.endsWith('。')).toBe(true);
  });

  it('reports a miss for an anchor the row does not carry, or a text not in the body', () => {
    const quotes = [{ id: 'q1', text: '9月加息', start: 0, end: 0 }, { id: 'q2', text: '不在正文里', start: 0, end: 0 }];
    expect(sourceHighlight({ body, quotes }, 'q3')).toBeNull();
    expect(sourceHighlight({ body, quotes }, 'q2')).toBeNull();
    expect(sourceHighlight({ body, quotes: [{ id: 'q1', text: '', start: 0, end: 0 }] }, 'q1')).toBeNull();
  });
});

describe('trackSourceOperation', () => {
  it('names the detail route and decodes the row with its body', () => {
    const operation = trackSourceOperation('w 1', 'src_2c9e0a1b');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/tracks/w%201/sources/src_2c9e0a1b');
    const row = {
      source_id: 'src_2c9e0a1b', provenance: 'summary',
      origin: { kind: 'plugin', plugin_id: 'p', tool: 'get_report', args_sha256: 'ab', args_canon: 'v1', content_id: '752972' },
      title: 'Mikko 全球市场日志 9-13', published_at: '2026-09-13', content_id: '752972',
      body_bytes: 12, body_sha256: 'cd', captured_at: '2026-09-14T00:00:00Z',
      quotes: [{ id: 'q1', text: 'abc', start: 0, end: 3 }], body: 'abc def',
      extra_field_the_kernel_added: true,
    };
    const decoded = trackSourceDetailSchema.parse(row);
    expect(decoded.provenance).toBe('summary');
    expect(decoded.body).toBe('abc def');
    expect(decoded.quotes[0]?.id).toBe('q1');
    expect('extra_field_the_kernel_added' in decoded).toBe(false);
    expect(() => trackSourceDetailSchema.parse({ ...row, provenance: 'guess' })).toThrow();
    expect(() => trackSourceDetailSchema.parse({ ...row, body: undefined })).toThrow();
  });
});
