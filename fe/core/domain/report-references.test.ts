import { expect, it } from 'vitest';
import { reportReferenceRepository, reportTextReferences, qualifiedReportFileReference } from './report-references.js';
import { parseReportFileLink, reportFilePathRelativeToRoot } from './report-file.js';

it('derives one repository only from explicit visible Markdown links', () => {
  expect(reportReferenceRepository(['[issue](https://github.com/example/project/issues/1)', '[PR](https://github.com/example/project/pull/2)']))
    .toEqual({ owner: 'example', name: 'project' });
  expect(reportReferenceRepository(['<!-- [x](https://github.com/hidden/project/issues/1) -->', '```md\n[x](https://github.com/code/project/issues/1)\n```', '`https://github.com/code/project/issues/1`'])).toBeNull();
  expect(reportReferenceRepository(['[a](https://github.com/one/project/issues/1)', '[b](https://github.com/two/project/issues/2)'])).toBeNull();
  expect(reportReferenceRepository(['issue #2420'])).toBeNull();
});

it('resolves issue and PR shorthands without changing their spelling', () => {
  const text = '调查 issue #2420；PR #2484 与 Pull request #9。';
  const parts = reportTextReferences(text, { owner: 'example', name: 'project' });
  expect(parts.map(part => part.text).join('')).toBe(text);
  expect(parts.filter(part => part.destination !== null).map(part => part.destination)).toEqual([
    'https://github.com/example/project/issues/2420', 'https://github.com/example/project/pull/2484', 'https://github.com/example/project/pull/9',
  ]);
});

it('does not guess repositories or accept invalid issue numbers/context', () => {
  for (const repository of [null, { owner: 'evil/other', name: 'project' }]) {
    expect(reportTextReferences('issue #2420', repository)).toEqual([{ text: 'issue #2420', destination: null }]);
  }
  for (const text of ['issue #0', 'PR #01', 'issue #9007199254740992']) {
    expect(reportTextReferences(text, { owner: 'example', name: 'project' })).toEqual([{ text, destination: null }]);
  }
});

it.each(['fe/core/track.ts:12-20', './docs/report.md', '../docs/report.md', '/repo/fe/track.ts:12,20-30'])('recognizes qualified source %s while keeping root admission authoritative', value => {
  expect(qualifiedReportFileReference(value)).toBe(true);
  const parts = reportTextReferences('查看 ' + value + '。', null);
  expect(parts.map(part => part.text).join('')).toBe('查看 ' + value + '。');
  expect(parts.find(part => part.destination)?.destination).toBe(value);
});

it('normalizes source line/range suffixes through the file owner', () => {
  expect(parseReportFileLink('fe/core/track.ts:12-20,25:3')).toEqual({ path: 'fe/core/track.ts' });
  const target = parseReportFileLink('../../outside.ts:1-2');
  expect(target).not.toBeNull();
  expect(target !== null && reportFilePathRelativeToRoot('/repo', target)).toBeNull();
});

it.each(['foo.bar', 'api_suite', '/api/tracks', 'cargo fmt --check', 'https://evil.invalid/a/code.ts', 'javascript:bad/code.ts', 'user@folder/code.ts'])('does not turn unrelated expression %s into a file destination', text => {
  expect(reportTextReferences(text, null)).toEqual([{ text, destination: null }]);
});
