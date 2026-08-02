import { describe, expect, it } from 'vitest';

import {
  extractOutline, fileViewerHeadingIdPolicy, parse, reportHeadingIdPolicy, type MarkdownDepth,
} from './public.js';

function outline(markdown: string, maxDepth: MarkdownDepth = 6) {
  const result = parse(markdown);
  expect(result.status).toBe('ready');
  if (result.status !== 'ready') return [];
  return extractOutline([{ context: undefined, ast: result.value }], {
    maxDepth,
    headingId: fileViewerHeadingIdPolicy,
    textPolicy: 'non-empty-heading-label',
    referenceText: 'source',
    traversal: 'line-level',
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

  it('resolves reference links and images and preserves undefined references literally', () => {
    const markdown = '# See [label][id] here\n# ![alt][id]\n# [label][id]\n# [missing][nope]\n\n[id]: /target "title"';
    const result = parse(markdown);
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(outline(markdown).map(({ id, text }) => ({ id, text }))).toEqual([
      { id: 'md-h-0', text: 'See [label][id] here' },
      { id: 'md-h-1', text: '![alt][id]' },
      { id: 'md-h-2', text: '[label][id]' },
      { id: 'md-h-3', text: '[missing][nope]' },
    ]);
    expect(extractOutline([{ context: { blockId: 'b_ref' }, ast: result.value }], {
      maxDepth: 2, headingId: reportHeadingIdPolicy, textPolicy: 'heading-label', referenceText: 'visible', traversal: 'recursive',
    }).map(({ id, text }) => ({ id, text }))).toEqual([
      { id: 'b_ref-h1', text: 'See label here' },
      { id: 'b_ref-h2', text: 'alt' },
      { id: 'b_ref-h3', text: 'label' },
      { id: 'b_ref-h4', text: '[missing][nope]' },
    ]);
    const referenceHeading = result.value.children[0];
    expect(Object.keys(referenceHeading ?? {}).sort()).toEqual(['children', 'depth', 'position', 'type']);
    expect(referenceHeading && 'children' in referenceHeading ? referenceHeading.children.map((child) => {
      const { position: _position, ...value } = child;
      void _position;
      return value;
    }) : []).toEqual([
      { type: 'text', value: 'See ' },
      {
        type: 'link', destination: '/target', title: 'title', referenceSource: '[label][id]',
        children: [{ type: 'text', value: 'label', position: { start: { line: 1, column: 8, offset: 7 }, end: { offset: 12 } } }],
      },
      { type: 'text', value: ' here' },
    ]);
    expect(referenceHeading && { type: referenceHeading.type, depth: 'depth' in referenceHeading ? referenceHeading.depth : undefined }).toEqual({
      type: 'heading',
      depth: 1,
    });
    const imageHeading = result.value.children[1];
    expect(Object.keys(imageHeading ?? {}).sort()).toEqual(['children', 'depth', 'position', 'type']);
    expect(imageHeading && 'children' in imageHeading ? imageHeading.children.map(({ position, ...child }) => {
      void position;
      return child;
    }) : [])
      .toEqual([{ type: 'image', alt: 'alt', destination: '/target', title: 'title', referenceSource: '![alt][id]' }]);
  });

  it('preserves source spelling and nested visible text in reference literals', () => {
    expect(outline('# [label][ID]\n# [*em* x][id]\n\n[ID]: /target').map(({ text }) => text)).toEqual([
      '[label][ID]',
      '[em x][id]',
    ]);
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
    expect(result.value.children[3] && 'children' in result.value.children[3] ? {
      type: result.value.children[3].type,
      children: result.value.children[3].children.map(({ position, ...child }) => {
        void position;
        return child;
      }),
    } : null).toEqual({
      type: 'paragraph',
      children: [{
        type: 'link', destination: 'b', title: null,
        children: [{ type: 'text', value: 'a', position: { start: { line: 7, column: 2, offset: 28 }, end: { offset: 29 } } }],
      }],
    });
    expect(result.value.children[4] && 'children' in result.value.children[4] ? {
      type: result.value.children[4].type,
      children: result.value.children[4].children.map(({ position, ...child }) => {
        void position;
        return child;
      }),
    } : null).toEqual({
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
    expect(result.value.children[2] && 'children' in result.value.children[2] ? {
      type: result.value.children[2].type,
      children: result.value.children[2].children.map(({ position, ...child }) => {
        void position;
        return child;
      }),
    } : null).toEqual({ type: 'paragraph', children: [{ type: 'text', value: '####### Seven' }] });
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

  it('does not diagnose table-like prose', () => {
    for (const markdown of ['a | b\ncross-reference | x-y', 'see foo | bar\nnote - baz | qux']) {
      const result = parse(markdown);
      expect(result.status).toBe('ready');
      if (result.status !== 'ready') continue;
      expect(result.value.diagnostics).toEqual([]);
    }
  });

  it.each([
    ['unclosed fence', '```md\n# not a heading', { kind: 'malformed', message: 'Unclosed fenced code block', line: 1 }],
    ['malformed table', '| a | b |\n| -- | --- | --- |\n# after', { kind: 'malformed', message: 'Malformed GFM table delimiter', line: 2 }],
    ['raw script', '<script>alert(1)</script>\n# after', { kind: 'unsafe-raw-html', message: 'Raw HTML requires sanitization', line: 1 }],
  ])('degrades %s without throwing and locks its diagnostic schema', (_name, markdown, diagnostic) => {
    expect(() => parse(markdown)).not.toThrow();
    const result = parse(markdown);
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).toContainEqual(diagnostic);
  });

  it('does not classify a prose-like table delimiter as a malformed GFM table', () => {
    const result = parse('| a | b |\n| -- | nope |\n# after');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).toEqual([]);
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

  it('keeps legal deeply indented inputs ready', () => {
    for (const markdown of [
      `    ${'>'.repeat(65)}`,
      `- item\n${' '.repeat(140)}continuation`,
      `${' '.repeat(130)}code`,
    ]) expect(parse(markdown).status).toBe('ready');
  });

  it('cannot hide adversarial nesting behind a backtick in a fence info string', () => {
    const markdown = `\`\`\`x\`\n${Array.from({ length: 100 }, (_, depth) => `${'  '.repeat(depth)}- x`).join('\n')}`;
    const started = Date.now();
    const result = parse(markdown);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(result.status).toBe('failed');
    if (result.status !== 'failed') return;
    expect(result.error.diagnostics).toContainEqual({
      kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: 66,
    });
  });

  const BYPASS_PREFIXES = Object.freeze([
    ['html script', '<script>\n```\n</script>\n'],
    ['html pre', '<pre>\n```\n</pre>\n'],
    ['html style', '<style>\n```\n</style>\n'],
    ['html comment', '<!--\n```\n-->\n'],
    ['html processing instruction', '<?target\n```\n?>\n'],
    ['html declaration', '<!DOCTYPE\n```\n>\n'],
    ['html cdata', '<![CDATA[\n```\n]]>\n'],
    ['html block tag', '<div>\n```\n</div>\n\n'],
    ['html complete tag', '<x-box>\n```\n\n'],
    ['closed backtick fence', '```\ninside\n```\n'],
    ['closed tilde fence', '~~~js\ninside\n~~~\n'],
    ['backtick info ambiguity', '```js`bad\n'],
    ['indented fence', '    ```\n'],
    ['blockquote fence', '> ```\n'],
    ['list fence', '- ```\n'],
  ] as const);

  it.each(BYPASS_PREFIXES)('fails closed for bypass prefix: %s', (_name, prefix) => {
    const adversarial = Array.from({ length: 250 }, (_, depth) => `${'  '.repeat(depth)}- x`).join('\n');
    const started = Date.now();
    const result = parse(prefix + adversarial);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(result.status).toBe('failed');
  });

  it('applies invalid backtick-info fence classification in diagnosticsFor', () => {
    const result = parse('```js`bad\n####### Seven');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics).toContainEqual({
      kind: 'malformed', message: 'ATX heading depth exceeds six', line: 2,
    });
  });

  it('keeps the first duplicate reference definition', () => {
    const result = parse('[x][id]\n\n[id]: /first\n[id]: /second');
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    const paragraph = result.value.children[0];
    const link = paragraph !== undefined && 'children' in paragraph ? paragraph.children[0] : undefined;
    expect(link?.type === 'link' ? link.destination : null).toBe('/first');
  });

  it('keeps repository-scale plain input ready', () => {
    expect(parse('x'.repeat(500_000)).status).toBe('ready');
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
