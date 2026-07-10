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
//   * Scheme + host are matched case-insensitively (`HTTPS://GITHUB.COM`
//     is the same authority per RFC 3986 — scheme and host are
//     case-insensitive) and `issue_url` is normalized to the lowercase
//     spelling. The path is NOT case-folded: owner/repo keep the case
//     the user pasted, and the literal `issues` segment must be
//     lowercase (fail-closed over guessing at GitHub's redirect rules).
//   * At most one trailing slash, then an optional query string or
//     fragment after the issue number — tolerated and stripped;
//     `issue_url` is normalized to the bare canonical form so the
//     kernel persists one spelling.
// Rejected: http:// (no silent upgrade), www.github.com, pull-request
// URLs (`/pull/<n>`), any suffix path after the issue number
// (`/issues/12/pull/99`, `/issues/12//`), missing/non-numeric issue
// numbers, leading-zero issue numbers (`/issues/07` is not GitHub's
// canonical spelling; silently normalizing would make `issue_url`
// disagree with the pasted text, so it fails instead), issue number 0,
// numbers past `Number.MAX_SAFE_INTEGER`, owner/repo segments outside
// GitHub's name charset (spaces, `%`-encoding, unicode, empty
// segments), and the path-traversal spellings `.` / `..` as a repo
// segment.

export interface ParsedIssueUrl {
  /** `owner/name`, e.g. `"keanji-x/neige-calm"`. */
  repo: string;
  /** Positive integer parsed from the `/issues/<n>` path segment. */
  issue_number: number;
  /** Normalized canonical URL (query/fragment/trailing slash stripped). */
  issue_url: string;
}

// Scheme + host, matched case-insensitively (see the ledger above); the
// capture is the pathname-and-beyond with its case intact.
const SCHEME_HOST_RE = /^https:\/\/github\.com(\/.*)$/i;

// Pathname must be exactly `/owner/repo/issues/<digits>` with at most
// one terminal slash before an optional `?` query or `#` fragment.
// Owner: GitHub usernames/orgs are alphanumeric + hyphens. Repo names
// additionally allow `.` and `_`. Anything outside that charset (spaces,
// `%2F` tricks, unicode) fails the match rather than round-tripping into
// `workflow_input.repo`. No `.*` after the number: `/issues/12/pull/99`
// must not parse as issue 12.
const PATH_RE =
  /^\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/issues\/([0-9]+)\/?(?:[?#].*)?$/;

/**
 * Parse a GitHub issue URL into the structured fields the
 * `issue-development` workflow's `input_schema` requires. Returns `null`
 * for anything that is not an https github.com issue URL — the form
 * surfaces that as an inline validation error and disables submit.
 */
export function parseGitHubIssueUrl(raw: string): ParsedIssueUrl | null {
  const host = SCHEME_HOST_RE.exec(raw.trim());
  if (!host) return null;
  const m = PATH_RE.exec(host[1]);
  if (!m) return null;
  const [, owner, name, digits] = m;
  // `.` and `..` sit inside the repo charset but are path traversal,
  // not repo names — reject. (The owner charset has no `.`, so only the
  // repo segment needs this check.)
  if (name === '.' || name === '..') return null;
  // Leading zeros: `/issues/07` is not GitHub's canonical spelling.
  // Rejected rather than normalized — see the ledger above.
  if (digits.length > 1 && digits.startsWith('0')) return null;
  const issueNumber = Number(digits);
  // `[0-9]+` already guarantees digits-only; reject 0 (GitHub issues
  // start at #1) and anything past the safe-integer range (the
  // kernel-side `type:"integer"` check would reject a float-encoded
  // overflow anyway — fail here, before the round trip).
  if (!Number.isSafeInteger(issueNumber) || issueNumber <= 0) return null;
  return {
    repo: `${owner}/${name}`,
    issue_number: issueNumber,
    issue_url: `https://github.com/${owner}/${name}/issues/${issueNumber}`,
  };
}
