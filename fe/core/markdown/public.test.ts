import { describe, expect, it } from 'vitest';

import { extractOutline, parse } from './public.js';

const ids = { version: 1 as const, createId: ({ globalOrdinal }: { globalOrdinal: number }) => `x-${globalOrdinal}` };

function outline(markdown: string, maxDepth: 1 | 2 | 3 | 4 | 5 | 6 = 6) {
  const result = parse(markdown);
  expect(result.status).toBe('ready');
  if (result.status !== 'ready') return [];
  return extractOutline(result.value, {
    maxDepth,
    headingId: ids,
    textPolicy: 'heading-label',
    context: undefined,
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

  it('keeps duplicate titles and gives each its own zero-based ordinal id', () => {
    expect(outline('# Same\n# Same').map(({ id, text }) => ({ id, text }))).toEqual([
      { id: 'x-0', text: 'Same' },
      { id: 'x-1', text: 'Same' },
    ]);
  });

  it('accepts depth one and six and rejects depth seven as a heading', () => {
    expect(outline('# One\n###### Six\n####### Seven').map(({ depth, text }) => ({ depth, text })))
      .toEqual([{ depth: 1, text: 'One' }, { depth: 6, text: 'Six' }]);
    expect(outline('# One\n## Two', 1).map(({ text }) => text)).toEqual(['One']);
  });

  it.each([
    ['unclosed fence', '```md\n# not a heading'],
    ['malformed table', '| a | b |\n| -- | nope |\n# after'],
    ['deep nesting', `${'> '.repeat(80)}# nested`],
    ['raw script', '<script>alert(1)</script>\n# after'],
  ])('degrades %s without throwing and reports a diagnostic', (_name, markdown) => {
    expect(() => parse(markdown)).not.toThrow();
    const result = parse(markdown);
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') return;
    expect(result.value.diagnostics.length).toBeGreaterThan(0);
  });
});
