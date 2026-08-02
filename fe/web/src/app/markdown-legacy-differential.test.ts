import { describe, expect, it } from 'vitest';

import {
  FILE_VIEWER_MAX_DEPTH,
  REPORT_MAX_DEPTH,
  extractOutline,
  fileViewerHeadingIdPolicy,
  parse,
  reportHeadingIdPolicy,
} from '../../../core/markdown/public.ts';

type LegacyHeading = Readonly<{ level: number; id: string; text: string }>;
type LegacyOutline = Readonly<{ blockId: string; label: string }>;
const fileViewerLegacyPath: string = new URL('../../../../web/src/cards/builtins/file-viewer-markdown-toc.tsx', import.meta.url).href;
const reportLegacyPath: string = new URL('../../../../web/src/pages/report-outline.ts', import.meta.url).href;
const { extractHeadings } = await import(fileViewerLegacyPath) as Readonly<{
  extractHeadings(markdown: string): LegacyHeading[];
}>;
const { deriveOutline } = await import(reportLegacyPath) as Readonly<{
  deriveOutline(blocks: readonly unknown[]): LegacyOutline[];
}>;

const CORPUS = Object.freeze([
  ['atx', '# Title\n## Usage'],
  ['atx closing', '### Closed ###'],
  ['setext h1', 'Title\n==='],
  ['setext h2', 'Title\n---'],
  ['blockquote', '# Root\n> ## Quoted\n## Tail'],
  ['list two spaces', '# Root\n- Setup\n  ## Install\n## Tail'],
  ['ordered list three spaces', '1. item\n   ## InList\n## Tail'],
  ['nested list', '- outer\n  - inner\n   ## Nested\n## Tail'],
  ['fenced backticks', '# Before\n```md\n## Hidden\n```\n## After'],
  ['fenced tildes', '~~~\n# Hidden\n~~~\n# Visible'],
  ['indented code', '    # Hidden\n# Visible'],
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
]);

const EXEMPTIONS = Object.freeze(new Set(['exempt strike', 'exempt inline html']));
const ACTIVE_CORPUS = CORPUS;

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

function legacyFileViewer(markdown: string) {
  return extractHeadings(markdown).map(({ level: depth, id, text }) => ({ depth, id, text }));
}

function legacyReport(markdown: string) {
  return deriveOutline([{
    id: 'block',
    kind: 'prose',
    payload: { markdown },
  }]).map(({ blockId: id, label: text }) => ({ id, text }));
}

describe('legacy markdown outline differential', () => {
  it('keeps the differential corpus substantive', () => {
    expect(ACTIVE_CORPUS.length).toBeGreaterThanOrEqual(20);
    expect([...EXEMPTIONS]).toEqual(['exempt strike', 'exempt inline html']);
  });

  it.each(ACTIVE_CORPUS.filter(([name]) => !EXEMPTIONS.has(name)))('%s matches both real legacy implementations', (_name, markdown) => {
    expect(normalized(markdown, 'file-viewer')).toEqual(legacyFileViewer(markdown));
    expect(normalized(markdown, 'report').map(({ id, text }) => ({ id, text })))
      .toEqual(legacyReport(markdown).map(({ id, text }) => ({ id, text })));
  });

  it.each(ACTIVE_CORPUS.filter(([name]) => EXEMPTIONS.has(name)))('%s remains an explicit established exemption', (_name, markdown) => {
    expect(normalized(markdown, 'file-viewer')).not.toEqual(legacyFileViewer(markdown));
    expect(normalized(markdown, 'report').map(({ id, text }) => ({ id, text })))
      .not.toEqual(legacyReport(markdown).map(({ id, text }) => ({ id, text })));
  });
});
