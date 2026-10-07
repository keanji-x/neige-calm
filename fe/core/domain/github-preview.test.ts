import { describe, expect, it } from 'vitest';
import { parseGitHubReferenceUrl, parseGitHubIssueUrl } from './issue-url.js';
import { githubPreviewOperation, githubPreviewSchema } from './github-preview.js';

describe('GitHub preview references', () => {
  it('admits Issue and PR citations without widening template inputs', () => {
    const target = parseGitHubReferenceUrl('https://github.com/owner/repo/pull/42?x=y#discussion');
    expect(target).toEqual({ owner: 'owner', name: 'repo', kind: 'pull', number: 42, url: 'https://github.com/owner/repo/pull/42' });
    expect(parseGitHubIssueUrl('https://github.com/owner/repo/pull/42')).toBeNull();
    expect(parseGitHubReferenceUrl('https://github.com/owner/repo/issues/42/')?.kind).toBe('issue');
    expect(githubPreviewOperation(target!).path).toBe('/api/github/preview?owner=owner&repo=repo&kind=pull&number=42');
  });
  it.each([
    'http://github.com/o/r/pull/1', 'https://github.com.evil.test/o/r/pull/1',
    'https://user@github.com/o/r/pull/1', 'https://github.com:443/o/r/pull/1',
    'https://github.com/o%2Fr/r/pull/1', 'https://github.com/o/../pull/1',
    'https://github.com/o/r/pull/1/files', 'https://github.com/o/r/pull/01',
    'https://github.com/o/r/pull/0', 'https://github.com/o/r/pull/9007199254740992',
    'javascript:alert(1)', 'https://github.com/o/r/issues/1/pull/2',
  ])('rejects %s', (url) => expect(parseGitHubReferenceUrl(url)).toBeNull());
  it('requires the full response contract', () => {
    expect(githubPreviewSchema.safeParse({ title: 'Title' }).success).toBe(false);
  });
});
