// GitHub issue URL → the structured fields the `issue-development` template's
// `template_input` requires (#1209).
//
// The kernel deliberately does no URL syntax work: its `input_schema` checks
// field *shapes* (`repo` is a string, `issue_number` an integer) and nothing
// else, so `repo` / `issue_number` must be structured at the entry surface.
// This is that derivation, and it is the whole reason the New wave dialog asks
// for one URL instead of three fields.
//
// A parallel parser exists in the legacy frontend. It is deliberately NOT
// imported: the two frontends share no module graph, and reaching across would
// be the first such edge. The *contract* is shared and the ledger below is
// copied from it on purpose — the accepted/rejected set is the thing that must
// agree, and a comment that restates it is cheaper to compare than a call.
//
// Accepted (deliberately narrow, fail-closed):
//   * `https://github.com/<owner>/<repo>/issues/<n>` — github.com only.
//     Enterprise hosts are rejected: the shipped workflow drives `gh` against
//     github.com, so an enterprise URL would create a wave whose repo
//     cross-check can never pass.
//   * Scheme and host match case-insensitively (RFC 3986 makes both
//     case-insensitive) and normalize to lowercase. The path is NOT
//     case-folded: owner/repo keep the pasted case and the literal `issues`
//     segment must be lowercase.
//   * At most one trailing slash, then an optional `?query` / `#fragment`,
//     both stripped — `issue_url` is normalized to the bare canonical form so
//     the kernel persists one spelling.
// Rejected: `http://` (no silent upgrade), `www.github.com`, pull-request URLs,
// any suffix path after the number (`/issues/12/pull/99`), missing/non-numeric
// numbers, leading zeros (`/issues/07` — normalizing would make `issue_url`
// disagree with the pasted text, so it fails instead), issue 0, numbers past
// `Number.MAX_SAFE_INTEGER`, owner/repo outside GitHub's name charset, and the
// traversal spellings `.` / `..` as a repo segment.

export type ParsedIssueUrl = Readonly<{
  /** `owner/name`, e.g. `"keanji-x/neige-calm"`. */
  repo: string;
  /** Positive integer from the `/issues/<n>` segment. */
  issue_number: number;
  /** Canonical URL — query, fragment and trailing slash stripped. */
  issue_url: string;
}>;

const SCHEME_HOST_RE = /^https:\/\/github\.com(\/.*)$/i;

// Owner: alphanumeric + hyphen. Repo additionally allows `.` and `_`. Anything
// else (spaces, `%2F` tricks, unicode) fails the match rather than round-
// tripping into `template_input.repo`. No `.*` after the number.
const PATH_RE = /^\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/issues\/([0-9]+)\/?(?:[?#].*)?$/;

/** `null` for anything that is not an https github.com issue URL. */
export function parseGitHubIssueUrl(raw: string): ParsedIssueUrl | null {
  const host = SCHEME_HOST_RE.exec(raw.trim());
  if (!host) return null;
  const matched = PATH_RE.exec(host[1]);
  if (!matched) return null;
  const [, owner, name, digits] = matched;
  // `.` and `..` sit inside the repo charset but are traversal, not names.
  // (The owner charset has no `.`, so only the repo segment needs this.)
  if (name === '.' || name === '..') return null;
  if (digits.length > 1 && digits.startsWith('0')) return null;
  const issueNumber = Number(digits);
  if (!Number.isSafeInteger(issueNumber) || issueNumber <= 0) return null;
  return {
    repo: `${owner}/${name}`,
    issue_number: issueNumber,
    issue_url: `https://github.com/${owner}/${name}/issues/${issueNumber}`,
  };
}
