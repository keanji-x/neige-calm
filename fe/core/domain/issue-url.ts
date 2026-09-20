// GitHub issue URL → the structured fields the `issue-development` template requires.
// Fail-closed: `https://github.com/<owner>/<repo>/issues/<n>` only, no enterprise hosts,
// no `http://`, no leading zeros; query, fragment and trailing slash are stripped.

export type ParsedIssueUrl = Readonly<{
  /** `owner/name`, e.g. `"keanji-x/neige-calm"`. */
  repo: string;
  /** Positive integer from the `/issues/<n>` segment. */
  issue_number: number;
  /** Canonical URL — query, fragment and trailing slash stripped. */
  issue_url: string;
}>;

const SCHEME_HOST_RE = /^https:\/\/github\.com(\/.*)$/i;

// Owner: alphanumeric + hyphen. Repo additionally allows `.` and `_`. No `.*` after the number.
const PATH_RE = /^\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/issues\/([0-9]+)\/?(?:[?#].*)?$/;

/** `null` for anything that is not an https github.com issue URL. */
export function parseGitHubIssueUrl(raw: string): ParsedIssueUrl | null {
  const host = SCHEME_HOST_RE.exec(raw.trim());
  if (!host) return null;
  const matched = PATH_RE.exec(host[1]);
  if (!matched) return null;
  const [, owner, name, digits] = matched;
  // `.` and `..` sit inside the repo charset but are traversal, not names.
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
