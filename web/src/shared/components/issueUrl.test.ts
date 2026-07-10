// parseGitHubIssueUrl unit tests — issue #891 slice ③ (design §5③).
//
// The parser is the single derivation point for the issue-dev wave
// form's `workflow_input` structured fields, so both halves matter:
// happy paths must normalize to the one canonical spelling, and hostile
// / near-miss inputs must all come back `null` (the form disables
// submit on `null`).

import { describe, it, expect } from 'vitest';
import { parseGitHubIssueUrl } from './issueUrl';

describe('parseGitHubIssueUrl — accepted', () => {
  it('parses a canonical issue URL', () => {
    expect(parseGitHubIssueUrl('https://github.com/keanji-x/neige-calm/issues/891')).toEqual({
      repo: 'keanji-x/neige-calm',
      issue_number: 891,
      issue_url: 'https://github.com/keanji-x/neige-calm/issues/891',
    });
  });

  it('tolerates surrounding whitespace (paste artifacts)', () => {
    expect(parseGitHubIssueUrl('  https://github.com/o/r/issues/7\n')).toEqual({
      repo: 'o/r',
      issue_number: 7,
      issue_url: 'https://github.com/o/r/issues/7',
    });
  });

  it('strips a trailing slash and normalizes issue_url', () => {
    const parsed = parseGitHubIssueUrl('https://github.com/o/r/issues/12/');
    expect(parsed?.issue_number).toBe(12);
    expect(parsed?.issue_url).toBe('https://github.com/o/r/issues/12');
  });

  it('strips a query string', () => {
    const parsed = parseGitHubIssueUrl(
      'https://github.com/o/r/issues/12?notification_referrer_id=abc',
    );
    expect(parsed?.issue_url).toBe('https://github.com/o/r/issues/12');
  });

  it('strips a fragment (e.g. #issuecomment deep link)', () => {
    const parsed = parseGitHubIssueUrl(
      'https://github.com/o/r/issues/12#issuecomment-123456',
    );
    expect(parsed?.issue_url).toBe('https://github.com/o/r/issues/12');
    expect(parsed?.issue_number).toBe(12);
  });

  it('accepts dots/underscores/hyphens in the repo name', () => {
    expect(parseGitHubIssueUrl('https://github.com/my-org/repo.name_x/issues/3')?.repo).toBe(
      'my-org/repo.name_x',
    );
  });
});

describe('parseGitHubIssueUrl — rejected', () => {
  it.each([
    ['empty string', ''],
    ['not a URL', 'issue 891'],
    ['bare repo (missing /issues/<n>)', 'https://github.com/o/r'],
    ['missing issue number', 'https://github.com/o/r/issues/'],
    ['non-numeric issue number', 'https://github.com/o/r/issues/abc'],
    ['pull-request URL', 'https://github.com/o/r/pull/42'],
    ['http (no silent https upgrade)', 'http://github.com/o/r/issues/1'],
    ['www host', 'https://www.github.com/o/r/issues/1'],
    ['GitHub Enterprise host', 'https://github.corp.example.com/o/r/issues/1'],
    ['unrelated forge', 'https://gitlab.com/o/r/issues/1'],
    ['lookalike host suffix', 'https://github.com.evil.example/o/r/issues/1'],
    ['owner with slash-encoded tricks', 'https://github.com/o%2Fx/r/issues/1'],
    ['owner with spaces', 'https://github.com/o wner/r/issues/1'],
    ['issue number 0', 'https://github.com/o/r/issues/0'],
    ['digits followed by junk in the number segment', 'https://github.com/o/r/issues/12abc'],
    ['extra path before issues', 'https://github.com/o/r/x/issues/12'],
  ])('rejects %s', (_label, input) => {
    expect(parseGitHubIssueUrl(input)).toBeNull();
  });

  it('rejects an issue number beyond the safe-integer range', () => {
    expect(
      parseGitHubIssueUrl('https://github.com/o/r/issues/999999999999999999999'),
    ).toBeNull();
  });
});
