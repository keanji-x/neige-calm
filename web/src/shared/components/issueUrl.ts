// GitHub issue URL → structured `workflow_input` fields — issue #891
// slice ③ (design §3.1).
//
// The issue-dev wave form asks the user for exactly two things: a GitHub
// issue URL and a working directory. Everything else in the
// `issue-development` workflow's `workflow_input` is derived, and this
// parser is the derivation: `{repo, issue_number, issue_url}` out of the
// pasted URL, entirely client-side. The kernel deliberately does no URL
// syntax work (design §6 decision 2 — "repo/issue_number are structured
// at the entry surface"); its input_schema only checks the field shapes.
//
// Accepted (deliberately narrow, fail-closed):
//   * `https://github.com/<owner>/<repo>/issues/<n>` — github.com only.
//     GitHub Enterprise hosts are rejected in v1: the shipped
//     `issue-development` workflow drives the `gh` forge tools against
//     github.com, so accepting an enterprise URL here would create a
//     wave that can never pass the workflow's repo cross-check.
//   * A trailing slash, query string, or fragment after the issue number
//     is tolerated and stripped — `issue_url` is normalized to the bare
//     canonical form so the kernel persists one spelling.
// Rejected: http:// (no silent upgrade), www.github.com, pull-request
// URLs (`/pull/<n>`), missing/non-numeric issue numbers, owner/repo
// segments outside GitHub's name charset, and issue number 0.

export interface ParsedIssueUrl {
  /** `owner/name`, e.g. `"keanji-x/neige-calm"`. */
  repo: string;
  /** Positive integer parsed from the `/issues/<n>` path segment. */
  issue_number: number;
  /** Normalized canonical URL (query/fragment/trailing slash stripped). */
  issue_url: string;
}

// Owner: GitHub usernames/orgs are alphanumeric + hyphens. Repo names
// additionally allow `.` and `_`. Anything outside that charset (spaces,
// `%2F` tricks, unicode) fails the match rather than round-tripping into
// `workflow_input.repo`.
const ISSUE_URL_RE =
  /^https:\/\/github\.com\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/issues\/(\d+)(?:[/?#].*)?$/;

/**
 * Parse a GitHub issue URL into the structured fields the
 * `issue-development` workflow's `input_schema` requires. Returns `null`
 * for anything that is not an https github.com issue URL — the form
 * surfaces that as an inline validation error and disables submit.
 */
export function parseGitHubIssueUrl(raw: string): ParsedIssueUrl | null {
  const m = ISSUE_URL_RE.exec(raw.trim());
  if (!m) return null;
  const [, owner, name, digits] = m;
  const issueNumber = Number(digits);
  // `\d+` already guarantees digits-only; reject 0 (GitHub issues start
  // at #1) and anything past the safe-integer range (the kernel-side
  // `type:"integer"` check would reject a float-encoded overflow anyway
  // — fail here, before the round trip).
  if (!Number.isSafeInteger(issueNumber) || issueNumber <= 0) return null;
  return {
    repo: `${owner}/${name}`,
    issue_number: issueNumber,
    issue_url: `https://github.com/${owner}/${name}/issues/${issueNumber}`,
  };
}
