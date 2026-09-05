import { afterEach, expect, it, vi } from 'vitest';
import { githubOwnershipRequest, githubSquashCommits, type GitHubRequest } from './github-squash';

function fixture() {
  const sha = 'a'.repeat(40);
  const original = 'b'.repeat(40);
  const commit = { sha, message: 'squash (#99999)', paths: ['frozen.txt'] };
  const pull = {
    number: 1478, merged_at: '2026-09-05', merge_commit_sha: sha,
    base: { ref: 'main', repo: { full_name: 'owner/repo' } },
    commits: 1, head: { sha: original },
  };
  const detail = {
    sha: original, commit: { message: 'OWNERSHIP-CHANGE: frozen.txt — approved fixture (#1478)' },
    parents: [{ sha: 'c'.repeat(40) }], files: [{ filename: 'frozen.txt', status: 'modified' }],
  };
  const pages: Record<string, unknown> = {
    [`/commits/${sha}/pulls?per_page=100&page=1`]: [pull],
    '/pulls/1478': pull,
    '/pulls/1478/commits?per_page=100&page=1': [{ sha: original }],
    [`/commits/${original}?per_page=100&page=1`]: detail,
  };
  const request = vi.fn((path: string) => {
    if (!(path in pages)) throw new Error(`unexpected request ${path}`);
    return Promise.resolve(pages[path]);
  });
  return { sha, original, commit, pull, detail, pages, request,
    run: () => githubSquashCommits(commit, 'owner/repo', request) };
}

afterEach(() => vi.unstubAllGlobals());
it('loads original evidence from the exact merged PR without trusting a subject number', async () => {
  const f = fixture();
  expect(await f.run()).toEqual([{ sha: f.original, message: f.detail.commit.message, paths: ['frozen.txt'] }]);
  expect(f.request).toHaveBeenCalledTimes(4);
});

it.each(['unmerged', 'wrong sha', 'wrong branch', 'wrong repository', 'absent'])('does not recover an %s association', async (kind) => {
  const f = fixture();
  if (kind === 'unmerged') Object.assign(f.pull, { merged_at: null });
  if (kind === 'wrong sha') f.pull.merge_commit_sha = 'd'.repeat(40);
  if (kind === 'wrong branch') f.pull.base.ref = 'other';
  if (kind === 'wrong repository') f.pull.base.repo.full_name = 'other/repo';
  if (kind === 'absent') f.pages[`/commits/${f.sha}/pulls?per_page=100&page=1`] = [];
  expect(await f.run()).toEqual([]);
  expect(f.request).toHaveBeenCalledTimes(1);
});

it('rejects ambiguous merged associations', async () => {
  const f = fixture();
  f.pages[`/commits/${f.sha}/pulls?per_page=100&page=1`] = [f.pull, { ...f.pull, number: 1479 }];
  await expect(f.run()).rejects.toThrow('ambiguous');
});

it.each(['number', 'merged_at', 'merge_commit_sha', 'branch', 'repository'])('rechecks PR detail identity: %s', async (field) => {
  const f = fixture();
  const changed = structuredClone(f.pull);
  if (field === 'number') changed.number = 1479;
  if (field === 'merged_at') Object.assign(changed, { merged_at: null });
  if (field === 'merge_commit_sha') changed.merge_commit_sha = 'd'.repeat(40);
  if (field === 'branch') changed.base.ref = 'other';
  if (field === 'repository') changed.base.repo.full_name = 'other/repo';
  f.pages['/pulls/1478'] = changed;
  await expect(f.run()).rejects.toThrow('identity changed');
});

it('paginates associations, original commits, and changed files', async () => {
  const f = fixture();
  f.pages[`/commits/${f.sha}/pulls?per_page=100&page=1`] = Array.from({ length: 100 }, (_, i) => ({ ...f.pull, number: i + 1, merged_at: null }));
  f.pages[`/commits/${f.sha}/pulls?per_page=100&page=2`] = [f.pull];
  const hashes = Array.from({ length: 101 }, (_, i) => (i + 1).toString(16).padStart(40, '0'));
  f.pull.commits = hashes.length;
  f.pull.head.sha = hashes.at(-1)!;
  f.pages['/pulls/1478/commits?per_page=100&page=1'] = hashes.slice(0, 100).map((sha) => ({ sha }));
  f.pages['/pulls/1478/commits?per_page=100&page=2'] = [{ sha: hashes[100] }];
  for (const sha of hashes) f.pages[`/commits/${sha}?per_page=100&page=1`] = { ...f.detail, sha };
  f.pages[`/commits/${hashes[0]}?per_page=100&page=1`] = {
    ...f.detail, sha: hashes[0], files: Array.from({ length: 100 }, (_, i) => ({ filename: `file-${i}`, status: 'modified' })),
  };
  f.pages[`/commits/${hashes[0]}?per_page=100&page=2`] = { ...f.detail, sha: hashes[0] };
  const result = await f.run();
  expect(result).toHaveLength(101);
  expect(result[0].paths).toHaveLength(101);
  expect(result[0].paths.at(-1)).toBe('frozen.txt');
  expect(result.at(-1)?.sha).toBe(hashes[100]);
});

it.each(['limit', 'empty', 'duplicate', 'head', 'extra'])('rejects incomplete original history: %s', async (kind) => {
  const f = fixture();
  if (kind === 'limit') f.pull.commits = 251;
  if (kind === 'empty') f.pages['/pulls/1478/commits?per_page=100&page=1'] = [];
  if (kind === 'duplicate') {
    f.pull.commits = 2;
    f.pages['/pulls/1478/commits?per_page=100&page=1'] = [{ sha: f.original }, { sha: f.original }];
  }
  if (kind === 'head') f.pull.head.sha = 'd'.repeat(40);
  if (kind === 'extra') f.pages['/pulls/1478/commits?per_page=100&page=1'] = [{ sha: f.original }, { sha: 'd'.repeat(40) }];
  await expect(f.run()).rejects.toThrow(/API limit|commit history/);
});

it('audits both paths of a rename', async () => {
  const f = fixture();
  f.detail.files = [{ filename: 'new.txt', status: 'renamed', previous_filename: 'frozen.txt' } as typeof f.detail.files[number]];
  expect((await f.run())[0].paths).toEqual(['new.txt', 'frozen.txt']);
});

it.each(['missing rename source', 'merge', 'wrong sha', 'missing files'])('fails closed on malformed commit evidence: %s', async (kind) => {
  const f = fixture();
  if (kind === 'missing rename source') f.detail.files[0].status = 'renamed';
  if (kind === 'merge') f.detail.parents.push({ sha: 'd'.repeat(40) });
  if (kind === 'wrong sha') f.detail.sha = 'd'.repeat(40);
  if (kind === 'missing files') Object.assign(f.detail, { files: undefined });
  await expect(f.run()).rejects.toThrow();
});

it('rejects the 3000 file truncation boundary', async () => {
  const f = fixture();
  for (let page = 1; page <= 30; page += 1) {
    f.pages[`/commits/${f.original}?per_page=100&page=${page}`] = {
      ...f.detail, files: Array.from({ length: 100 }, (_, i) => ({ filename: `${page}-${i}`, status: 'modified' })),
    };
  }
  await expect(f.run()).rejects.toThrow('3000 file API limit');
});

it('rejects inconsistent messages across file pages', async () => {
  const f = fixture();
  f.detail.files = Array.from({ length: 100 }, (_, i) => ({ filename: `file-${i}`, status: 'modified' }));
  f.pages[`/commits/${f.original}?per_page=100&page=2`] = { ...f.detail, commit: { message: 'different' }, files: [] };
  await expect(f.run()).rejects.toThrow('inconsistent ownership commit');
});

it('propagates API errors instead of accepting missing evidence', async () => {
  const request: GitHubRequest = () => Promise.reject(new Error('API unavailable'));
  await expect(githubSquashCommits(fixture().commit, 'owner/repo', request)).rejects.toThrow('API unavailable');
});

it('bounds authenticated requests to GitHub and rejects redirects', async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response('[]', { status: 200 }));
  vi.stubGlobal('fetch', fetcher);
  const request = githubOwnershipRequest({ repository: 'owner/repo', token: 'fixture-token' });
  const path = `/commits/${'a'.repeat(40)}/pulls?per_page=100&page=1`;
  expect(await request(path)).toEqual([]);
  const [url, options] = fetcher.mock.calls[0];
  expect(url).toBe(`https://api.github.com/repos/owner/repo${path}`);
  expect(options?.redirect).toBe('error');
  expect(options?.signal).toBeInstanceOf(AbortSignal);
  expect(options?.headers).toMatchObject({ Authorization: 'Bearer fixture-token' });
  await expect(request('/../../evil')).rejects.toThrow('invalid ownership API path');
  expect(fetcher).toHaveBeenCalledTimes(1);
});

it.each(['', '../repo', 'owner/..', 'owner/repo/extra', 'owner/repo?x'])('rejects invalid repository %s', (repository) => {
  expect(() => githubOwnershipRequest({ repository, token: '' })).toThrow('valid GitHub repository');
});

it('supports public read-only replay without a token and reports HTTP failures without response contents', async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response('sensitive response', { status: 403 }));
  vi.stubGlobal('fetch', fetcher);
  const request = githubOwnershipRequest({ repository: 'owner/repo', token: '' });
  await expect(request('/pulls/1478')).rejects.toThrow('ownership GitHub API failed (403)');
  expect(fetcher).toHaveBeenCalledWith(expect.any(String), expect.objectContaining({
    headers: { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' },
  }));
});
