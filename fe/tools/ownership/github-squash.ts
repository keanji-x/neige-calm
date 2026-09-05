import { z } from 'zod';
import type { OwnershipCommit } from './validator.ts';

export interface GitHubOwnershipConfig { repository: string; token: string }
export type GitHubRequest = (path: string) => Promise<unknown>;

// No API-provided URLs are followed, including pagination links and redirects.
export function githubOwnershipRequest(config: GitHubOwnershipConfig): GitHubRequest {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(config.repository)
    || config.repository.split('/').some((part) => part === '.' || part === '..')) {
    throw new Error('ownership recovery requires a valid GitHub repository');
  }
  return async (path) => {
    if (!/^\/(?:commits\/[0-9a-f]{40}(?:\/pulls)?|pulls\/\d+(?:\/commits)?)(?:\?per_page=100&page=\d+)?$/.test(path)) {
      throw new Error('invalid ownership API path');
    }
    const response = await fetch(`https://api.github.com/repos/${config.repository}${path}`, {
      headers: {
        Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28',
        ...(config.token ? { Authorization: `Bearer ${config.token}` } : {}),
      },
      redirect: 'error',
      signal: AbortSignal.timeout(15_000),
    });
    if (!response.ok) throw new Error(`ownership GitHub API failed (${response.status}) for ${path}`);
    const body: unknown = await response.json();
    return body;
  };
}

export async function githubSquashCommits(
  commit: OwnershipCommit,
  repository: string,
  request: GitHubRequest,
): Promise<OwnershipCommit[]> {
  const shaSchema = z.string().regex(/^[0-9a-f]{40}$/);
  shaSchema.parse(commit.sha);
  const pullSchema = z.object({
    number: z.number().int().positive(),
    merged_at: z.string().nullable(),
    merge_commit_sha: z.string().nullable(),
    base: z.object({ ref: z.string(), repo: z.object({ full_name: z.string() }) }),
  });
  const associated = [];
  for (let page = 1; ; page += 1) {
    if (page > 100) throw new Error('ownership PR associations exceed pagination limit');
    const batch = z.array(pullSchema).parse(await request(`/commits/${commit.sha}/pulls?per_page=100&page=${page}`));
    associated.push(...batch);
    if (batch.length < 100) break;
  }
  const matches = associated.filter((pull) => pull.merged_at !== null
    && pull.merge_commit_sha === commit.sha && pull.base.ref === 'main'
    && pull.base.repo.full_name.toLowerCase() === repository.toLowerCase());
  if (matches.length === 0) return [];
  if (matches.length !== 1) throw new Error(`ambiguous ownership PR association for ${commit.sha}`);
  const number = matches[0].number;
  const pull = pullSchema.extend({
    commits: z.number().int().positive(), head: z.object({ sha: shaSchema }),
  }).parse(await request(`/pulls/${number}`));
  if (pull.number !== number || pull.merged_at === null || pull.merge_commit_sha !== commit.sha
    || pull.base.ref !== 'main' || pull.base.repo.full_name.toLowerCase() !== repository.toLowerCase()) {
    throw new Error('ownership PR identity changed while loading evidence');
  }
  // The PR commits endpoint returns at most 250 commits. Never audit a prefix.
  if (pull.commits > 250) throw new Error('ownership PR exceeds the 250 commit API limit');
  const hashes: string[] = [];
  for (let page = 1; hashes.length < pull.commits; page += 1) {
    const batch = z.array(z.object({ sha: shaSchema })).parse(await request(`/pulls/${number}/commits?per_page=100&page=${page}`));
    if (batch.length === 0) throw new Error('incomplete ownership PR commit history');
    hashes.push(...batch.map(({ sha }) => sha));
  }
  if (hashes.length !== pull.commits || new Set(hashes).size !== hashes.length || hashes.at(-1) !== pull.head.sha) {
    throw new Error('incomplete or inconsistent ownership PR commit history');
  }
  const result: OwnershipCommit[] = [];
  for (const sha of hashes) {
    const paths: string[] = [];
    let message = '';
    for (let page = 1; ; page += 1) {
      const detail = z.object({
        sha: shaSchema,
        commit: z.object({ message: z.string() }),
        parents: z.array(z.object({ sha: shaSchema })).max(1),
        files: z.array(z.object({
          filename: z.string().min(1), status: z.string(), previous_filename: z.string().min(1).optional(),
        })),
      }).parse(await request(`/commits/${sha}?per_page=100&page=${page}`));
      if (detail.sha !== sha || (page > 1 && detail.commit.message !== message)) {
        throw new Error('inconsistent ownership commit evidence');
      }
      message = detail.commit.message;
      for (const file of detail.files) {
        paths.push(file.filename);
        if (file.status === 'renamed') {
          if (!file.previous_filename) throw new Error('ownership rename is missing its original path');
          paths.push(file.previous_filename);
        }
      }
      // GitHub caps a commit's files at 3,000; reaching the cap cannot prove completeness.
      if (page >= 30 && detail.files.length === 100) throw new Error('ownership commit reaches the 3000 file API limit');
      if (detail.files.length < 100) break;
    }
    result.push({ sha, message, paths: [...new Set(paths)] });
  }
  console.log(`ownership ${commit.sha}: recovered original commits from ${repository}#${number}`);
  return result;
}
