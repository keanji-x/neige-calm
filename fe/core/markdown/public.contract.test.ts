import { describe, expect, expectTypeOf, it } from 'vitest';

import {
  extractOutline,
  parse,
  sanitizeAstPolicy,
  type HeadingIdPolicy,
  type HeadingOutline,
  type MarkdownParseResult,
  type NormalizedHeading,
  type NormalizedMarkdownAst,
  type TextPolicy,
} from './public.js';

describe('core/markdown public contract', () => {
  it('uses the shared ready/failed data channel and freezes the GFM AST vocabulary', () => {
    expectTypeOf<MarkdownParseResult['status']>().toEqualTypeOf<'ready' | 'failed'>();
    expectTypeOf<Extract<MarkdownParseResult, { status: 'failed' }>['error']['kind']>()
      .toEqualTypeOf<'markdown-parse'>();
    expectTypeOf<NormalizedMarkdownAst['dialect']>().toEqualTypeOf<'gfm'>();
    expectTypeOf<NormalizedMarkdownAst['children'][number]['type']>().toEqualTypeOf<
      'heading' | 'paragraph' | 'code' | 'table' | 'thematic-break'
    >();
    expectTypeOf<TextPolicy>().toEqualTypeOf<'heading-label'>();

    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- heading depth seven is outside CommonMark's H1-H6 domain.
      const invalidHeading: NormalizedHeading = { type: 'heading', depth: 7, children: [] };
      void invalidHeading;
      // @ts-expect-error -- text extraction is a closed policy, not a callback escape hatch.
      const invalidTextPolicy: TextPolicy = () => 'anything';
      void invalidTextPolicy;
    }
  });

  it('passes the node, zero-based global/local ordinals, and caller context to the id policy', () => {
    const seen: Array<Readonly<{ depth: number; global: number; local: number; blockId: string }>> = [];
    const headingId: HeadingIdPolicy<Readonly<{ blockId: string }>> = {
      version: 1,
      createId(input) {
        seen.push({
          depth: input.heading.depth,
          global: input.globalOrdinal,
          local: input.localOrdinal,
          blockId: input.context.blockId,
        });
        return `${input.context.blockId}-h${input.localOrdinal}`;
      },
    };
    const parsed = parse('# One\n\n## Two');
    expect(parsed.status).toBe('ready');
    if (parsed.status !== 'ready') return;

    const outline = extractOutline(parsed.value, {
      maxDepth: 2,
      headingId,
      textPolicy: 'heading-label',
      context: { blockId: 'b_1234' },
    });

    expect(outline).toEqual([
      { depth: 1, id: 'b_1234-h0', text: 'One', globalOrdinal: 0, localOrdinal: 0 },
      { depth: 2, id: 'b_1234-h1', text: 'Two', globalOrdinal: 1, localOrdinal: 1 },
    ] satisfies HeadingOutline[]);
    expect(seen).toEqual([
      { depth: 1, global: 0, local: 0, blockId: 'b_1234' },
      { depth: 2, global: 1, local: 1, blockId: 'b_1234' },
    ]);
  });

  it('produces both legacy outline schemes from the same extractor', () => {
    const parsed = parse('# Top\n## Child\n### Detail\n#### Leaf\n##### Hidden');
    expect(parsed.status).toBe('ready');
    if (parsed.status !== 'ready') return;

    const report = extractOutline(parsed.value, {
      maxDepth: 2,
      textPolicy: 'heading-label',
      context: { blockId: 'b_ab12' },
      headingId: {
        version: 1,
        createId: ({ context, localOrdinal }) => `${context.blockId}-h${localOrdinal}`,
      },
    });
    const fileViewer = extractOutline(parsed.value, {
      maxDepth: 4,
      textPolicy: 'heading-label',
      context: undefined,
      headingId: { version: 1, createId: ({ globalOrdinal }) => `md-h-${globalOrdinal}` },
    });

    expect(report.map(({ depth, id }) => ({ depth, id }))).toEqual([
      { depth: 1, id: 'b_ab12-h0' },
      { depth: 2, id: 'b_ab12-h1' },
    ]);
    expect(fileViewer.map(({ depth, id }) => ({ depth, id }))).toEqual([
      { depth: 1, id: 'md-h-0' },
      { depth: 2, id: 'md-h-1' },
      { depth: 3, id: 'md-h-2' },
      { depth: 4, id: 'md-h-3' },
    ]);
  });

  it('removes raw HTML through a platform-independent AST policy', () => {
    const parsed = parse('<script>alert(1)</script>\n\n# Safe');
    expect(parsed.status).toBe('ready');
    if (parsed.status !== 'ready') return;

    const safe = sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' });
    expect(safe.children).toEqual([
      { type: 'paragraph', children: [{ type: 'text', value: '<script>alert(1)</script>' }] },
      { type: 'heading', depth: 1, children: [{ type: 'text', value: 'Safe' }] },
    ]);
  });
});
