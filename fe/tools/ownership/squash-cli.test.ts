import { execFileSync, spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { expect, it } from 'vitest';

const cases = [
  { label: 'associated wrapped trailers', path: 'fe/core/api/generated/openapi.json', sourceApproved: true, finalTrailer: true, association: true, apiFailure: false, status: 0, error: '' },
  { label: 'missing durable trailer', path: 'fe/core/api/generated/openapi.json', sourceApproved: true, finalTrailer: false, association: true, apiFailure: false, status: 1, error: 'final commit message does not preserve' },
  { label: 'missing source trailer', path: 'fe/core/api/generated/openapi.json', sourceApproved: false, finalTrailer: true, association: true, apiFailure: false, status: 1, error: 'original PR commits fail audit' },
  { label: 'wrapped direct push', path: 'fe/core/api/generated/openapi.json', sourceApproved: true, finalTrailer: true, association: false, apiFailure: false, status: 1, error: 'cannot audit direct ownership push' },
  { label: 'unavailable association API', path: 'fe/core/api/generated/openapi.json', sourceApproved: true, finalTrailer: true, association: true, apiFailure: true, status: 1, error: 'API unavailable' },
  { label: 'raw replacement-character path', path: 'fe/core/api/bad\uFFFD.ts', sourceApproved: true, finalTrailer: true, association: false, apiFailure: false, status: 1, error: 'non-canonical changed path' },
] as const;

it.each(cases)('audits $label through both CLI entry points', ({
  path, sourceApproved, finalTrailer, association, apiFailure, status, error,
}) => {
  const repository = mkdtempSync(join(tmpdir(), 'ownership-cli-'));
  const feRoot = resolve(import.meta.dirname, '../..');
  const git = (...args: string[]) => execFileSync('git', args, { cwd: repository, encoding: 'utf8', stdio: 'pipe' }).trim();
  try {
    cpSync(feRoot, join(repository, 'fe'), {
      recursive: true,
      filter: (source) => !['node_modules', 'dist', '.cache'].includes(source.split('/').at(-1)!),
    });
    git('init', '--initial-branch=main');
    git('config', 'user.name', 'Ownership fixture');
    git('config', 'user.email', 'ownership@example.invalid');
    git('add', '.');
    git('commit', '-m', 'base');
    const base = git('rev-parse', 'HEAD');
    git('switch', '-c', 'feature');
    const trailer = `OWNERSHIP-CHANGE: ${path} — approved fixture (#1478)`;
    const wrappedTrailer = `OWNERSHIP-CHANGE: ${path} — approved fixture\n(#1478)`;
    writeFileSync(join(repository, path), '{}\n');
    const message = `change${sourceApproved ? `\n\n${trailer}` : ''}`;
    git('add', '.');
    git('commit', '-m', message);
    const source = git('rev-parse', 'HEAD');
    git('switch', 'main');
    git('merge', '--squash', 'feature');
    git('commit', '-m', `squash (#1478)${finalTrailer ? `\n\n${wrappedTrailer}` : ''}`);
    const head = git('rev-parse', 'HEAD');
    const pull = {
      number: 1478, merged_at: '2026-09-05', merge_commit_sha: head,
      base: { ref: 'main', repo: { full_name: 'fixture/repo' } }, commits: 1, head: { sha: source },
    };
    const pages = {
      [`/commits/${head}/pulls?per_page=100&page=1`]: association ? [pull] : [],
      '/pulls/1478': pull,
      '/pulls/1478/commits?per_page=100&page=1': [{ sha: source }],
      [`/commits/${source}?per_page=100&page=1`]: {
        sha: source, commit: { message }, parents: [{ sha: base }], files: [{ filename: path, status: 'modified' }],
      },
    };
    const shim = join(repository, 'github-fixture.mjs');
    writeFileSync(shim, `const pages = ${JSON.stringify(pages)};\nglobalThis.fetch = async (url) => {\n`
      + `${apiFailure ? "throw new Error('API unavailable');" : ''}\n`
      + `const path = String(url).replace('https://api.github.com/repos/fixture/repo', '');\n`
      + `if (!(path in pages)) throw new Error('unexpected API request: ' + path);\n`
      + `return new Response(JSON.stringify(pages[path]), { status: 200 });\n};\n`);
    symlinkSync(join(feRoot, 'node_modules'), join(repository, 'fe/node_modules'), 'dir');
    for (const checker of ['check-readonly-change-requests.mjs', 'check-ownership-trailers.mjs']) {
      const result = spawnSync(process.execPath, ['--import', shim, join(repository, 'fe/tools/ownership', checker)], {
        cwd: repository, encoding: 'utf8',
        env: {
          PATH: process.env.PATH,
          OWNERSHIP_EVENT_NAME: 'push', OWNERSHIP_BASE_SHA: base, OWNERSHIP_HEAD_SHA: head,
          OWNERSHIP_PUSH_FORCED: 'false', GITHUB_REPOSITORY: 'fixture/repo',
        },
      });
      const output = result.stdout + result.stderr;
      expect(result.error, checker).toBeUndefined();
      expect(result.status, `${checker}\n${output}`).toBe(status);
      if (association && !apiFailure) expect(output).toContain('recovered original commits from fixture/repo#1478');
      if (error) expect(output).toContain(error);
    }
  } finally {
    rmSync(repository, { recursive: true, force: true });
  }
}, 30_000);
