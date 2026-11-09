import { describe, expect, expectTypeOf, it } from 'vitest';

import {
  FILE_VIEWER_MAX_DEPTH,
  REPORT_MAX_DEPTH,
  extractOutline,
  fileViewerHeadingIdPolicy,
  parse,
  reportHeadingIdPolicy,
  sanitizeAstPolicy,
  type HeadingIdPolicy,
  type MarkdownParseResult,
  type NormalizedHeading,
  type NormalizedMarkdownAst,
  type SafeMarkdownAst,
  type TextPolicy,
} from './public.js';

function ready(markdown: string): NormalizedMarkdownAst {
  const result = parse(markdown);
  expect(result.status).toBe('ready');
  if (result.status !== 'ready') throw new Error('expected ready markdown');
  return result.value;
}

describe('core/markdown public contract', () => {
  it('freezes the parse channel, GFM vocabulary, and policy domains', () => {
    expectTypeOf<MarkdownParseResult['status']>().toEqualTypeOf<'ready' | 'failed'>();
    expectTypeOf<Extract<MarkdownParseResult, { status: 'failed' }>['error']['kind']>()
      .toEqualTypeOf<'markdown-parse'>();
    expectTypeOf<NormalizedMarkdownAst['dialect']>().toEqualTypeOf<'gfm'>();
    expectTypeOf<NormalizedMarkdownAst['sourceLines']>().toEqualTypeOf<readonly string[]>();
    expectTypeOf<NormalizedMarkdownAst['children'][number]['type']>().toEqualTypeOf<
      'heading' | 'paragraph' | 'code' | 'math' | 'blockquote' | 'list' | 'html' | 'table' | 'thematicBreak'
    >();
    expectTypeOf<HeadingIdPolicy<unknown>['version']>().toEqualTypeOf<1>();
    expectTypeOf<TextPolicy>().toEqualTypeOf<'heading-label' | 'non-empty-heading-label'>();
    expectTypeOf<SafeMarkdownAst['children'][number]['type']>().not.toEqualTypeOf<'html'>();
    expectTypeOf<SafeMarkdownAst['sourceLines']>().toEqualTypeOf<readonly string[]>();

    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- heading depth seven is outside CommonMark's H1-H6 domain.
      const invalidHeading: NormalizedHeading = { type: 'heading', depth: 7, children: [] };
      void invalidHeading;
      // @ts-expect-error -- text extraction is a closed policy, not a callback escape hatch.
      const invalidTextPolicy: TextPolicy = () => 'anything';
      void invalidTextPolicy;
      // @ts-expect-error -- outline maxDepth cannot be below the H1-H6 domain.
      extractOutline([], { maxDepth: 0, headingId: fileViewerHeadingIdPolicy, textPolicy: 'heading-label', referenceText: 'source', traversal: 'line-level' });
      // @ts-expect-error -- outline maxDepth cannot exceed the H1-H6 domain.
      extractOutline([], { maxDepth: 7, headingId: fileViewerHeadingIdPolicy, textPolicy: 'heading-label', referenceText: 'source', traversal: 'line-level' });
    }
  });

  it('resets report local ordinals per block while file-viewer ordinals stay global', () => {
    const inputs = [
      { context: { blockId: 'b_one' }, ast: ready('# One\n## Two') },
      { context: { blockId: 'b_two' }, ast: ready('# Three\n## Four') },
    ];
    const report = extractOutline(inputs, {
      maxDepth: REPORT_MAX_DEPTH,
      headingId: reportHeadingIdPolicy,
      textPolicy: 'heading-label',
      referenceText: 'visible',
      traversal: 'recursive',
    });
    const fileViewer = extractOutline(inputs.map(({ ast }) => ({ context: undefined, ast })), {
      maxDepth: FILE_VIEWER_MAX_DEPTH,
      headingId: fileViewerHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'source',
      traversal: 'line-level',
    });

    expect(report.map(({ depth, id, text, globalOrdinal, localOrdinal }) => ({
      depth, id, text, globalOrdinal, localOrdinal,
    }))).toEqual([
      { depth: 1, id: 'b_one-h1', text: 'One', globalOrdinal: 0, localOrdinal: 0 },
      { depth: 2, id: 'b_one-h2', text: 'Two', globalOrdinal: 1, localOrdinal: 1 },
      { depth: 1, id: 'b_two-h1', text: 'Three', globalOrdinal: 2, localOrdinal: 0 },
      { depth: 2, id: 'b_two-h2', text: 'Four', globalOrdinal: 3, localOrdinal: 1 },
    ]);
    expect(fileViewer.map(({ id, globalOrdinal, localOrdinal }) => ({ id, globalOrdinal, localOrdinal }))).toEqual([
      { id: 'md-h-0', globalOrdinal: 0, localOrdinal: 0 },
      { id: 'md-h-1', globalOrdinal: 1, localOrdinal: 1 },
      { id: 'md-h-2', globalOrdinal: 2, localOrdinal: 0 },
      { id: 'md-h-3', globalOrdinal: 3, localOrdinal: 1 },
    ]);
  });

  it('exports both depth and id policies with exact legacy schemes', () => {
    const ast = ready('# Top\n## Child\n### Detail\n#### Leaf\n##### Hidden');
    const report = extractOutline([{ context: { blockId: 'b_ab12' }, ast }], {
      maxDepth: REPORT_MAX_DEPTH,
      headingId: reportHeadingIdPolicy,
      textPolicy: 'heading-label',
      referenceText: 'visible',
      traversal: 'recursive',
    });
    const fileViewer = extractOutline([{ context: undefined, ast }], {
      maxDepth: FILE_VIEWER_MAX_DEPTH,
      headingId: fileViewerHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'source',
      traversal: 'line-level',
    });

    expect(report.map(({ id }) => id)).toEqual(['b_ab12-h1', 'b_ab12-h2']);
    expect(fileViewer.map(({ id }) => id)).toEqual(['md-h-0', 'md-h-1', 'md-h-2', 'md-h-3']);
  });

  it('parameterizes traversal to preserve both distinct legacy outlines', () => {
    const ast = ready('# Alpha\n\n> ## Quoted\n\n- item\n  ## InList\n\n> - nested\n>   ## Deep\n\n## Beta');
    const report = extractOutline([{ context: { blockId: 'b_nested' }, ast }], {
      maxDepth: REPORT_MAX_DEPTH, headingId: reportHeadingIdPolicy, textPolicy: 'heading-label', referenceText: 'visible', traversal: 'recursive',
    });
    const fileViewer = extractOutline([{ context: undefined, ast }], {
      maxDepth: FILE_VIEWER_MAX_DEPTH, headingId: fileViewerHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'source',
      traversal: 'line-level',
    });

    expect(report.map(({ text, id, localOrdinal }) => ({ text, id, localOrdinal }))).toEqual([
      { text: 'Alpha', id: 'b_nested-h1', localOrdinal: 0 },
      { text: 'Quoted', id: 'b_nested-h2', localOrdinal: 1 },
      { text: 'InList', id: 'b_nested-h3', localOrdinal: 2 },
      { text: 'Deep', id: 'b_nested-h4', localOrdinal: 3 },
      { text: 'Beta', id: 'b_nested-h5', localOrdinal: 4 },
    ]);
    expect(fileViewer.map(({ text, id, globalOrdinal }) => ({ text, id, globalOrdinal }))).toEqual([
      { text: 'Alpha', id: 'md-h-0', globalOrdinal: 0 },
      { text: 'InList', id: 'md-h-1', globalOrdinal: 1 },
      { text: 'Beta', id: 'md-h-2', globalOrdinal: 2 },
    ]);
  });

  it('preserves source positions on the AST and extracted headings', () => {
    const markdown = 'intro\n\n## Positioned\n';
    const ast = ready(markdown);
    const heading = ast.children[1];
    expect(heading && 'depth' in heading ? {
      type: heading.type, depth: heading.depth, position: heading.position,
    } : null).toEqual({
      type: 'heading', position: { start: { line: 3, column: 1, offset: 7 }, end: { offset: 20 } },
      depth: 2,
    });
    const outline = extractOutline([{ context: undefined, ast }], {
      maxDepth: FILE_VIEWER_MAX_DEPTH, headingId: fileViewerHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'source',
      traversal: 'line-level',
    });
    expect(outline[0]?.position).toEqual({ start: { line: 3, column: 1, offset: 7 }, end: { offset: 20 } });
  });

  it('removes raw HTML nodes through a narrowed safe AST', () => {
    const ast = ready('<script>alert(1)</script>\n\n# Safe <i>label</i>');
    expect(ast.children.some(({ type }) => type === 'html')).toBe(true);
    const safe = sanitizeAstPolicy(ast, { rawHtml: 'drop' });
    expect(safe.children.map((child) => ({
      type: child.type,
      ...('depth' in child ? { depth: child.depth } : {}),
      ...('children' in child ? {
        children: child.children.map(({ position, ...inline }) => {
          void position;
          return inline;
        }),
      } : {}),
    }))).toEqual([
      { type: 'heading', depth: 1, children: [{ type: 'text', value: 'Safe ' }, { type: 'text', value: 'label' }] },
    ]);
    expectTypeOf(safe.position.start.offset).toEqualTypeOf<number>();
    const heading = safe.children[0];
    if (heading?.type === 'heading') expectTypeOf(heading.position.start.offset).toEqualTypeOf<number>();
  });

  it('removes raw HTML at every nested block level at runtime', () => {
    const safe = sanitizeAstPolicy(ready('> - item\n>\n>   <span>unsafe</span>'), { rawHtml: 'drop' });
    const serialized = JSON.stringify(safe.children);
    expect(serialized).not.toContain('"type":"html"');
    expect(serialized).not.toContain('<span>');
  });
});


it.each(['  ', '\t', ' \t '])('promotes standalone display math with trailing whitespace %j and preserves source positions', (suffix) => {
  const source = `$$x^2$$${suffix}`;
  expect(ready(source).children).toEqual([{
    type: 'math', value: 'x^2',
    position: { start: { line: 1, column: 1, offset: 0 }, end: { offset: source.length } },
  }]);
});

it.each([
  '[item $5] costs $10\n\n[item $5]: /url',
  '[item $5]: /url "$10"\n\nSee [item $5]',
  '[shown][item $5] costs $10\n\n[item $5]: /url',
  '![item $5] costs $10\n\n[item $5]: /url',
])('restores currency in reference identifiers and labels: %s', (source) => {
  const block = ready(source).children[0];
  expect(block?.type).toBe('paragraph');
  if (block?.type !== 'paragraph') throw new Error('expected paragraph');
  const reference = block.children.find(node => node.type === 'link' || node.type === 'image');
  expect(reference).toMatchObject({ destination: '/url' });
  expect(JSON.stringify(block)).not.toContain('£');
});


it.each([
  ['[item $5] with', 'link', 1],
  ['[item $5] costs $10 and', 'link', 1],
  ['![item $5] costs $10 and', 'image', 1],
  ['[shown][item $5] with', 'link', 1],
  ['[item $5][] with', 'link', 1],
  ['[item $5] with', 'link', 2],
] as const)('resolves currency references beside real math: %s (%s, %i)', (prefix, type, count) => {
  for (const formula of ['$x$', '$$\nx\n$$']) {
    const source = `${prefix}${formula === '$x$' ? ' ' : '\n\n'}${formula}${count === 2 ? '\n\nSee [item $5]' : ''}\n\n[item $5]: /url`;
    const ast = ready(source);
    const references = ast.children.flatMap(block => block.type === 'paragraph'
      ? block.children.filter(node => node.type === type) : []);
    expect(references).toHaveLength(count);
    for (const reference of references) expect(reference).toMatchObject({ type, destination: '/url' });
    expect(JSON.stringify(ast)).toContain(`"type":"${formula === '$x$' ? 'inlineMath' : 'math'}","value":"x"`);
    expect(JSON.stringify(ast)).not.toContain('£');
  }
});


it.each([
  ['[item \\$5] with $x$\n\n[item \\$5]: /url', 'link', 'item $5', true],
  ['![item \\$5] with $x$\n\n[item \\$5]: /url', 'image', 'item $5', true],
  ['[shown \\$5][ref]\n\n[ref]: /url', 'link', 'shown $5', false],
  ['[shown \\$5][item \\$5] with $x$\n\n[item \\$5]: /url', 'link', 'shown $5', true],
  ['[item \\$5][] with $x$\n\n[item \\$5]: /url', 'link', 'item $5', true],
  ['[item \\[1\\] $5] with $x$\n\n[item \\[1\\] $5]: /url', 'link', 'item [1] $5', true],
] as const)('preserves escaped currency reference syntax: %s', (source, type, label, hasMath) => {
  const ast = ready(source);
  const block = ast.children[0];
  if (block?.type !== 'paragraph') throw new Error('expected paragraph');
  expect(ast.children).toHaveLength(1);
  const reference = block.children.find(node => node.type === type);
  expect(reference).toMatchObject({ type, destination: '/url' });
  if (reference?.type === 'link') expect(reference.children).toMatchObject([{ type: 'text', value: label }]);
  if (reference?.type === 'image') expect(reference.alt).toBe(label);
  expect(block.children.filter(node => node.type === 'inlineMath').map(node => node.value))
    .toEqual(hasMath ? ['x'] : []);
  expect(JSON.stringify(ast)).not.toContain('£');
});
