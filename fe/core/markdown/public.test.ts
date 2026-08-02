import { describe, expect, it } from 'vitest';

import { extractOutline, fileViewerHeadingIdPolicy, parse, type MarkdownDepth } from './public.js';

function outline(markdown: string, maxDepth: MarkdownDepth = 6) {
  const result = parse(markdown);
  expect(result.status).toBe('ready');
  if (result.status !== 'ready') return [];
  return extractOutline([{ context: undefined, ast: result.value }], {
    maxDepth,
    headingId: fileViewerHeadingIdPolicy,
    textPolicy: 'non-empty-heading-label',
  });
}

describe('core/markdown behavior', () => {
  it('recognizes ATX and setext headings while excluding fenced content', () => {
    expect(outline('# ATX\n\nSetext\n---\n\n```md\n# fenced\n```').map(({ depth, text }) => ({ depth, text })))
      .toEqual([{ depth: 1, text: 'ATX' }, { depth: 2, text: 'Setext' }]);
  });

  it('uses image alt and inline-code literal text in heading labels', () => {
    expect(outline('## A ![diagram](asset.png) with `x < y` and **bold**')[0]?.text)
      .toBe('A diagram with x < y and bold');
  });

  it('keeps visible CommonMark characters and omits empty file-viewer headings before numbering', () => {
    expect(outline('# snake_case\n# C\\*\n# price * tax\n# <i></i>\n# Last').map(({ id, text }) => ({ id, text }))).toEqual([
      { id: 'md-h-0', text: 'snake_case' },
      { id: 'md-h-1', text: 'C*' },
      { id: 'md-h-2', text: 'price * tax' },
      { id: 'md-h-3', text: 'Last' },
    ]);
  });

  it('normalizes GFM blocks and inline links without losing destinations', () => {
    const result = parse('- item one\n\n> quote\n\n1. n\n\n[a](b)\n\nleft line\nright line\n\n| a | b |\n| - | - |\n| 1 | 2 |');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.children.map(({ type }) => type)).toEqual([
      'list', 'blockquote', 'list', 'paragraph', 'paragraph', 'table',
    ]);
    expect(result.value.children[3]).toMatchObject({
      type: 'paragraph',
      children: [{ type: 'link', destination: 'b', title: null, children: [{ type: 'text', value: 'a' }] }],
    });
    expect(result.value.children[4]).toMatchObject({
      type: 'paragraph',
      children: [{ type: 'text', value: 'left line\nright line' }],
    });
    expect(result.value.diagnostics).toEqual([]);
  });

  it('downgrades depth seven to paragraph text and emits the exact diagnostic', () => {
    const result = parse('# One\n###### Six\n####### Seven');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.children.map(({ type }) => type)).toEqual(['heading', 'heading', 'paragraph']);
    expect(result.value.children[2]).toMatchObject({ type: 'paragraph', children: [{ type: 'text', value: '####### Seven' }] });
    expect(result.value.diagnostics).toContainEqual({
      kind: 'malformed', message: 'ATX heading depth exceeds six', line: 3,
    });
  });

  it('does not diagnose heading-like or table-like text inside a fence', () => {
    const result = parse('```md\n####### Seven\n| -- | nope |\n```');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).toEqual([]);
  });

  it.each([
    ['unclosed fence', '```md\n# not a heading', { kind: 'malformed', message: 'Unclosed fenced code block', line: 1 }],
    ['malformed table', '| a | b |\n| -- | nope |\n# after', { kind: 'malformed', message: 'Malformed GFM table delimiter', line: 2 }],
    ['raw script', '<script>alert(1)</script>\n# after', { kind: 'unsafe-raw-html', message: 'Raw HTML requires sanitization', line: 1 }],
  ])('degrades %s without throwing and locks its diagnostic schema', (_name, markdown, diagnostic) => {
    expect(() => parse(markdown)).not.toThrow();
    const result = parse(markdown);
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).toContainEqual(diagnostic);
  });

  it.each([
    ['unclosed fence', '```md\nbody'],
    ['table', '| a | b |\n| --- | --- |\n| 1 |'],
    ['raw script', '<script>alert(1)</script>'],
    ['unclosed link', '[label](destination'],
    ['BOM', '\uFEFF# title'],
    ['CRLF', '# title\r\n\r\nbody'],
    ['empty', ''],
    ['long line', `# ${'x'.repeat(100_000)}`],
  ])('does not throw for malformed/stress input: %s', (_name, markdown) => {
    expect(() => parse(markdown)).not.toThrow();
    expect(parse(markdown).status).toBe('ready');
  });

  it('rejects deeply nested lists before parsing within the runtime budget', () => {
    const markdown = Array.from({ length: 1_000 }, (_, depth) => `${'  '.repeat(depth)}- x`).join('\n');
    const started = Date.now();
    const result = parse(markdown);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(result.status).toBe('failed');
    if (result.status !== 'failed') return;
    expect(result.error.diagnostics).toContainEqual({
      kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: 65,
    });
  });

  it('rejects oversized source before parsing', () => {
    const result = parse('x'.repeat(2_000_001));
    expect(result.status).toBe('failed');
    if (result.status !== 'failed') return;
    expect(result.error.diagnostics).toEqual([
      { kind: 'limit-exceeded', message: 'Markdown source exceeds 2000000 characters', line: 1 },
    ]);
  });

  it('uses each HTML node position for duplicate raw HTML diagnostics', () => {
    const result = parse('<div>same</div>\n\ntext\n\n<div>same</div>');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics.filter(({ kind }) => kind === 'unsafe-raw-html').map(({ line }) => line))
      .toEqual([1, 5]);
  });

  it('does not treat a fenced prefix with trailing content as a closing fence', () => {
    const result = parse('```md\n```notclose\n```');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).not.toContainEqual(expect.objectContaining({
      message: 'Unclosed fenced code block',
    }));
  });
});
