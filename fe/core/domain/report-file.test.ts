import { describe, expect, it } from 'vitest';

import {
  parseReportFileLink, parseWorkspaceRelativeFilePath, reportFilePathRelativeToRoot,
  resolveReportFilePath,
} from './report-file.js';

describe('parseReportFileLink', () => {
  it('normalizes a workspace-relative Markdown destination', () => {
    expect(parseReportFileLink('./src/../README%20first.md#L12'))
      .toEqual({ path: 'README first.md' });
    expect(parseReportFileLink('fe/web/src/main.tsx'))
      .toEqual({ path: 'fe/web/src/main.tsx' });
    expect(parseReportFileLink('file:///srv/work/src/main.rs:42:7'))
      .toEqual({ path: '/srv/work/src/main.rs' });
    expect(parseReportFileLink('docs/100%2520done.md'))
      .toEqual({ path: 'docs/100%20done.md' });
  });

  it('rejects destinations that are not files beneath the workspace root', () => {
    for (const destination of [
      '', '.', '..',
      'https://example.com/x', '//example.com/x',
      '#section', '?download=1', 'neige://wave/w1', 'javascript:alert(1)',
    ]) {
      expect(parseReportFileLink(destination), destination).toBeNull();
    }
  });

  it('keeps parent segments as candidates until a document base is known', () => {
    expect(parseReportFileLink('../secret.txt')).toEqual({ path: '../secret.txt' });
    expect(parseReportFileLink('src/../../secret.txt')).toEqual({ path: '../secret.txt' });
    const target = parseReportFileLink('../secret.txt');
    if (target === null) throw new Error('parent target did not parse');
    expect(reportFilePathRelativeToRoot('/repo', target)).toBeNull();
  });

  it('keeps route and history paths relative even though report links may be absolute', () => {
    expect(parseWorkspaceRelativeFilePath('src/main.rs')).toEqual({ path: 'src/main.rs' });
    expect(parseWorkspaceRelativeFilePath('/srv/work/src/main.rs')).toBeNull();
    expect(parseWorkspaceRelativeFilePath('docs/100%20done.md'))
      .toEqual({ path: 'docs/100%20done.md' });
  });

  it('fails closed on malformed escapes and control characters', () => {
    expect(parseReportFileLink('bad%ZZ.txt')).toBeNull();
    expect(parseReportFileLink('bad%00.txt')).toBeNull();
    expect(parseReportFileLink('bad\\path.txt')).toBeNull();
  });
});

describe('resolveReportFilePath', () => {
  it('places the normalized relative path beneath an absolute workspace root', () => {
    expect(resolveReportFilePath('/srv/work/', { path: 'src/main.rs' }))
      .toBe('/srv/work/src/main.rs');
    expect(resolveReportFilePath('/', { path: 'tmp/a.txt' })).toBe('/tmp/a.txt');
    expect(resolveReportFilePath('/srv/work', { path: '/srv/work/src/main.rs' }))
      .toBe('/srv/work/src/main.rs');
    expect(reportFilePathRelativeToRoot('/srv/work', { path: '/srv/work/src/main.rs' }))
      .toBe('src/main.rs');
    const percent = parseReportFileLink('docs/100%2520done.md');
    if (percent === null) throw new Error('percent path did not parse');
    expect(resolveReportFilePath('/srv/work', percent)).toBe('/srv/work/docs/100%20done.md');
    expect(reportFilePathRelativeToRoot('/srv/work', percent)).toBe('docs/100%20done.md');
  });

  it('resolves links from an opened Markdown file before applying the root fence', () => {
    const sibling = parseReportFileLink('./spec.md');
    const parent = parseReportFileLink('../README.md');
    if (sibling === null || parent === null) throw new Error('relative Markdown links did not parse');
    expect(reportFilePathRelativeToRoot('/repo', sibling, 'docs')).toBe('docs/spec.md');
    expect(reportFilePathRelativeToRoot('/repo', parent, 'docs')).toBe('README.md');
    expect(reportFilePathRelativeToRoot('/repo', parent, '')).toBeNull();
  });

  it('refuses an absent or non-absolute workspace root', () => {
    expect(resolveReportFilePath('', { path: 'README.md' })).toBeNull();
    expect(resolveReportFilePath('relative', { path: 'README.md' })).toBeNull();
    expect(resolveReportFilePath('/srv/work', { path: '/srv/other/secret' })).toBeNull();
    expect(resolveReportFilePath('/srv/work', { path: '/srv/work-other/secret' })).toBeNull();
  });
});
