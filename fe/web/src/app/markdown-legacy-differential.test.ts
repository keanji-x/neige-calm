// @vitest-environment node

import { describe, expect, it } from 'vitest';

import {
  FILE_VIEWER_MAX_DEPTH,
  REPORT_MAX_DEPTH,
  extractOutline,
  fileViewerHeadingIdPolicy,
  parse,
  reportHeadingIdPolicy,
  sanitizeAstPolicy,
} from '../../../core/markdown/public.ts';

const CORPUS = Object.freeze([
  ['atx', '# Title\n## Usage'],
  ['atx closing', '### Closed ###'],
  ['setext h1', 'Title\n==='],
  ['setext h2', 'Title\n---'],
  ['blockquote', '# Root\n> ## Quoted\n## Tail'],
  ['list two spaces', '# Root\n- Setup\n  ## Install\n## Tail'],
  ['ordered list three spaces', '1. item\n   ## InList\n## Tail'],
  ['nested list', '- outer\n  - inner\n   ## Nested\n## Tail'],
  ['list heading four spaces', '# T\n- Setup\n    ## Deep\n## Tail'],
  ['nested list heading four spaces', '# T\n- outer\n  - inner\n    ## Nested\n## Tail'],
  ['fenced backticks', '# Before\n```md\n## Hidden\n```\n## After'],
  ['fenced tildes', '~~~\n# Hidden\n~~~\n# Visible'],
  ['indented code', '    # Hidden\n# Visible'],
  ['tab heading', '# Root\n\t## Tab\n## Tail'],
  ['tab space heading', '# Root\n\t ## Tab\n## Tail'],
  ['space tab heading', '# Root\n \t## Tab\n## Tail'],
  ['two spaces tab heading', '# Root\n  \t## Tab\n## Tail'],
  ['tab list then heading', '# Root\n-\ta\n\t## Tab\n## Tail'],
  ['table cell hash', '| value |\n| --- |\n| # cell |\n# Visible'],
  ['html block', '<div>block</div>\n\n# Visible'],
  ['reference full case', '# [label][ID]\n\n[ID]: /target'],
  ['reference nested inline', '# [*em* x][id]\n\n[id]: /target'],
  ['reference collapsed', '# [label][]\n\n[label]: /target'],
  ['reference shortcut', '# [label]\n\n[label]: /target'],
  ['reference image', '# ![alt][IMG]\n\n[IMG]: /image'],
  ['inline link and image', '# [link](/x) ![alt](/y)'],
  ['inline code', '# Use `x < y`'],
  ['emphasis strong', '# *one* **two**'],
  ['literal punctuation', '# price * tax and snake_case'],
  ['blank before thematic break', 'candidate\n\n---\n# Visible'],
  ['crlf', '# One\r\n\r\n## Two'],
  // Established migrations: GFM strike contributes visible text and inline HTML is stripped.
  ['exempt strike', '# ~~gone~~ kept'],
  ['exempt inline html', '# Safe <i>label</i>'],
] as const);

function normalized(markdown: string, policy: 'report' | 'file-viewer') {
  const result = parse(markdown);
  expect(result.status).toBe('ready');
  if (result.status !== 'ready') return [];
  const report = policy === 'report';
  return extractOutline([{
    context: report ? { blockId: 'block' } : undefined,
    ast: result.value,
  }], {
    maxDepth: report ? REPORT_MAX_DEPTH : FILE_VIEWER_MAX_DEPTH,
    headingId: report ? reportHeadingIdPolicy : fileViewerHeadingIdPolicy,
    textPolicy: report ? 'heading-label' : 'non-empty-heading-label',
    referenceText: report ? 'visible' : 'source',
    traversal: report ? 'recursive' : 'line-level',
  }).map(({ depth, id, text }) => ({ depth, id, text }));
}

// Captured before retiring the legacy application. These expectations keep the
// established maintained-frontend behavior independent of deleted source.
describe('markdown outline retirement regression', () => {
  it('preserves the file-viewer outline when the public sanitize and outline APIs are composed', () => {
    const markdown = '- x\n    ## Deep\n## Tail';
    const result = parse(markdown);
    expect(result.status).toBe('ready');
    if (result.status !== 'ready') throw new Error('expected ready markdown');
    const options = {
      maxDepth: FILE_VIEWER_MAX_DEPTH,
      headingId: fileViewerHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label' as const,
      referenceText: 'source' as const,
      traversal: 'line-level' as const,
    };
    const direct = extractOutline([{ context: undefined, ast: result.value }], options);
    const sanitized = extractOutline([{
      context: undefined,
      ast: sanitizeAstPolicy(result.value, { rawHtml: 'drop' }),
    }], options);

    expect(sanitized).toEqual(direct);
  });


  it.each(CORPUS)('%s preserves both outline policies', (_name, markdown) => {
    expect(normalized(markdown, 'file-viewer')).toMatchSnapshot('file-viewer');
    expect(normalized(markdown, 'report')).toMatchSnapshot('report');
  });
});
