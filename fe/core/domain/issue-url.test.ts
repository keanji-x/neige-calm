import { describe, expect, it } from 'vitest';

import { parseGitHubIssueUrl } from './issue-url.js';

describe('parseGitHubIssueUrl — accepted', () => {
  it('parses the canonical shape into the three workflow_input fields', () => {
    expect(parseGitHubIssueUrl('https://github.com/keanji-x/neige-calm/issues/1209')).toEqual({
      repo: 'keanji-x/neige-calm',
      issue_number: 1209,
      issue_url: 'https://github.com/keanji-x/neige-calm/issues/1209',
    });
  });

  it('trims surrounding whitespace — the field is pasted into, not typed', () => {
    expect(parseGitHubIssueUrl('  https://github.com/o/r/issues/7\n')?.issue_number).toBe(7);
  });

  it('strips a trailing slash, a query and a fragment from issue_url', () => {
    for (const raw of [
      'https://github.com/o/r/issues/12/',
      'https://github.com/o/r/issues/12?utm_source=x',
      'https://github.com/o/r/issues/12#issuecomment-1',
    ]) {
      expect(parseGitHubIssueUrl(raw)?.issue_url).toBe('https://github.com/o/r/issues/12');
    }
  });

  it('folds scheme and host case but leaves owner/repo alone', () => {
    expect(parseGitHubIssueUrl('HTTPS://GitHub.COM/My-Org/Repo.Name_x/issues/3')).toEqual({
      repo: 'My-Org/Repo.Name_x',
      issue_number: 3,
      issue_url: 'https://github.com/My-Org/Repo.Name_x/issues/3',
    });
  });
});

describe('parseGitHubIssueUrl — rejected, fail-closed', () => {
  it.each([
    ['http, no silent upgrade', 'http://github.com/o/r/issues/1'],
    ['www host', 'https://www.github.com/o/r/issues/1'],
    ['enterprise host', 'https://github.example.com/o/r/issues/1'],
    ['pull request', 'https://github.com/o/r/pull/1'],
    ['uppercase issues segment', 'https://github.com/o/r/ISSUES/1'],
    ['suffix path after the number', 'https://github.com/o/r/issues/12/pull/99'],
    ['double trailing slash', 'https://github.com/o/r/issues/12//'],
    ['missing number', 'https://github.com/o/r/issues/'],
    ['non-numeric number', 'https://github.com/o/r/issues/abc'],
    ['leading zero', 'https://github.com/o/r/issues/07'],
    ['issue zero', 'https://github.com/o/r/issues/0'],
    ['past MAX_SAFE_INTEGER', 'https://github.com/o/r/issues/9007199254740993'],
    ['percent-encoded separator', 'https://github.com/o%2Fr/x/issues/1'],
    ['empty owner', 'https://github.com//r/issues/1'],
    ['repo is a dot', 'https://github.com/o/./issues/1'],
    ['repo is dot-dot', 'https://github.com/o/../issues/1'],
    ['not a url at all', 'ship the thing'],
    ['empty', ''],
  ])('rejects %s', (_label, raw) => {
    expect(parseGitHubIssueUrl(raw)).toBeNull();
  });
});
