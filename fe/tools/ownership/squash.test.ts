import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  gitOwnershipCommits, ownershipCommitsForEvent, resolveOwnershipBase, validateOwnership,
  type OwnershipCommit,
} from './validator';

const manifest = [{ path: 'frozen.txt', type: 'file' as const, owner: 'fixture', readonly: true }];
const trailer = 'OWNERSHIP-CHANGE: frozen.txt — approved fixture change (#1478)';
let repository: string;
let base: string;
function git(...args: string[]): string {
  return execFileSync('git', args, { cwd: repository, encoding: 'utf8', stdio: 'pipe' }).trim();
}
function change(content: string, message: string): void {
  writeFileSync(join(repository, 'frozen.txt'), content);
  git('add', '.');
  git('commit', '-m', message);
}
function squash(): OwnershipCommit[] {
  git('switch', 'main');
  git('merge', '--squash', 'feature');
  git('commit', '-m', 'Squashed change (#1478)');
  const head = git('rev-parse', 'HEAD');
  return gitOwnershipCommits(repository, resolveOwnershipBase(repository, base, head, 'push'), head);
}
beforeEach(() => {
  repository = mkdtempSync(join(tmpdir(), 'ownership-squash-'));
  git('init', '--initial-branch=main');
  git('config', 'user.name', 'Ownership fixture');
  git('config', 'user.email', 'ownership@example.invalid');
  change('base', 'base');
  base = git('rev-parse', 'HEAD');
  git('switch', '-c', 'feature');
});
afterEach(() => rmSync(repository, { recursive: true, force: true }));

it('accepts a real squash push with approved original branch commits', async () => {
  change('approved', `change\n\n${trailer}`);
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash();
  expect(validateOwnership(manifest, [], pushed)).toHaveLength(1);
  const recovered = await ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source));
  expect(validateOwnership(manifest, [], recovered)).toEqual([]);
  expect(pushed[0].message).not.toContain(trailer);
});

it('rejects a real squash push with unapproved original branch commits', async () => {
  change('unapproved', 'change');
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash();
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source)))
    .rejects.toThrow('original PR commits');
});

it('rejects a later unapproved edit to the same approved path', async () => {
  change('approved', `change\n\n${trailer}`);
  change('unapproved', 'second change');
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash();
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source)))
    .rejects.toThrow('original PR commits');
});

it('does not borrow a trailer from a commit that did not change that path', async () => {
  change('approved', `change\n\n${trailer}`);
  const pushed = squash();
  const recovered = await ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve([
    { sha: 'unrelated', message: trailer, paths: ['ordinary.txt'] },
  ]));
  expect(validateOwnership(manifest, [], recovered)).toHaveLength(1);
});

it('keeps an unmatched direct push red', async () => {
  change('approved', `change\n\n${trailer}`);
  const pushed = squash();
  const recovered = await ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve([]));
  expect(validateOwnership(manifest, [], recovered)).toHaveLength(1);
});

it.each(['pull_request', undefined])('does not recover evidence for %s', async (event) => {
  change('unapproved', 'change');
  const commits = gitOwnershipCommits(repository, base);
  const recover = vi.fn(() => Promise.resolve([]));
  expect(await ownershipCommitsForEvent(event, () => commits, manifest, recover)).toBe(commits);
  expect(recover).not.toHaveBeenCalled();
});

it('does not request GitHub evidence for complete trailers or ordinary paths', async () => {
  change('approved', `change\n\n${trailer}`);
  const commits = gitOwnershipCommits(repository, base);
  commits.push({ sha: 'ordinary', message: 'ordinary', paths: ['ordinary.txt'] });
  const recover = vi.fn(() => Promise.reject(new Error('offline')));
  expect(await ownershipCommitsForEvent('push', () => commits, manifest, recover)).toEqual(commits);
  expect(recover).not.toHaveBeenCalled();
});

it('recovers partial trailers and keeps approvals scoped to each pushed commit', async () => {
  const entries = [...manifest, { ...manifest[0], path: 'second.txt' }];
  const commits = [
    { sha: 'one', message: trailer, paths: ['frozen.txt', 'second.txt'] },
    { sha: 'two', message: 'unapproved', paths: ['frozen.txt'] },
  ];
  const recover = vi.fn((commit: OwnershipCommit) => Promise.resolve(commit.sha === 'one' ? [{
    sha: 'source', message: 'OWNERSHIP-CHANGE: second.txt — approved fixture change (#1478)', paths: ['second.txt'],
  }] : []));
  const recovered = await ownershipCommitsForEvent('push', () => commits, entries, recover);
  expect(validateOwnership(entries, [], recovered)).toEqual([{
    rule: 'readonly-change-trailer', message: 'two changes frozen frozen.txt without an OWNERSHIP-CHANGE trailer',
  }]);
  expect(recover.mock.calls.map(([commit]) => commit.sha)).toEqual(['one', 'two']);
});

it('fails closed when evidence cannot be loaded', async () => {
  change('unapproved', 'change');
  const pushed = squash();
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.reject(new Error('API unavailable'))))
    .rejects.toThrow('API unavailable');
});
