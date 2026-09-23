import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  gitOwnershipCommits, ownershipCommitsForEvent, resolveOwnershipBase,
  type OwnershipCommit,
} from './validator';

const manifest = [{ path: 'frozen.txt', type: 'file' as const, owner: 'fixture', readonly: true }];
const trailer = 'OWNERSHIP-CHANGE: frozen.txt — approved fixture change (#1478)';
const realTrailer = "OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode task.gate_result's optional status_detail and target (VerifyTarget / evidence / sample / phase / reason discriminated unions) (#1727)";
const realWrappedTrailer = "OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode task.gate_result's\noptional status_detail and target (VerifyTarget / evidence / sample /\nphase / reason discriminated unions) (#1727)";
const earlyFoldTrailer = 'OWNERSHIP-CHANGE: frozen.txt — reason abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWX final (#1478)';
const earlyFoldWrappedTrailer = 'OWNERSHIP-CHANGE: frozen.txt — reason\nabcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWX final (#1478)';
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
function squash(message = 'Squashed change (#1478)'): OwnershipCommit[] {
  git('switch', 'main');
  git('merge', '--squash', 'feature');
  git('commit', '-m', message);
  const head = git('rev-parse', 'HEAD');
  return gitOwnershipCommits(repository, resolveOwnershipBase(repository, base, head, 'push'), head);
}
function syntheticAudit(message: string, sourceMessage = trailer): Promise<readonly OwnershipCommit[]> {
  const pushed = [{ sha: 'squash', message, paths: ['frozen.txt'] }];
  const source = [{ sha: 'source', message: sourceMessage, paths: ['frozen.txt'] }];
  return ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source));
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

it('accepts the real dccc140ff three-line 72-column trailer without rewriting the squash message', async () => {
  const entries = [{ path: 'fe/core/api/schemas.ts', type: 'file' as const, owner: 'fixture', readonly: true }];
  const pushed = [{ sha: 'squash', message: realWrappedTrailer, paths: ['fe/core/api/schemas.ts'] }];
  const source = [{ sha: 'source', message: realTrailer, paths: ['fe/core/api/schemas.ts'] }];
  const audited = await ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve(source));
  expect(audited).toBe(pushed);
  expect(audited[0].message).toBe(pushed[0].message);
  expect(audited[0].message).not.toContain(realTrailer);
});

it('accepts a long token that forces the 72-column wrapper to fold early', async () => {
  await expect(syntheticAudit(earlyFoldWrappedTrailer, earlyFoldTrailer)).resolves.toBeDefined();
});

it('counts an astral emoji as one code point at the 72-column boundary', async () => {
  const token = `😀${'x'.repeat(40)}`;
  const source = `OWNERSHIP-CHANGE: frozen.txt — ${token} (#1478)`;
  const durable = `OWNERSHIP-CHANGE: frozen.txt — ${token}\n(#1478)`;
  await expect(syntheticAudit(durable, source)).resolves.toBeDefined();
});

it('places a single token longer than 72 code points on its own line', async () => {
  const token = 'x'.repeat(73);
  const source = `OWNERSHIP-CHANGE: frozen.txt — ${token} (#1478)`;
  const durable = `OWNERSHIP-CHANGE: frozen.txt —\n${token}\n(#1478)`;
  await expect(syntheticAudit(durable, source)).resolves.toBeDefined();
});

it('accepts CRLF source and durable messages without changing wrap semantics', async () => {
  const pushed = [{
    sha: 'squash', message: `squash\r\n\r\n${earlyFoldWrappedTrailer.replaceAll('\n', '\r\n')}\r\n`, paths: ['frozen.txt'],
  }];
  const source = [{ sha: 'source', message: `change\r\n\r\n${earlyFoldTrailer}\r\n`, paths: ['frozen.txt'] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source)))
    .resolves.toBe(pushed);
});

it('accepts an associated squash with the exact canonical single-line trailer', async () => {
  await expect(syntheticAudit(trailer)).resolves.toBeDefined();
});

it('rejects a squash whose durable message omits a source trailer', async () => {
  change('approved', `change\n\n${trailer}`);
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash();
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source)))
    .rejects.toThrow('final commit message does not preserve source trailers');
});

it('audits original commits even when the durable message has a canonical trailer', async () => {
  change('unapproved', 'change');
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash(`Squashed change (#1478)\n\n${trailer}`);
  const recover = vi.fn(() => Promise.resolve(source));
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, recover))
    .rejects.toThrow('original PR commits fail audit');
  expect(recover).toHaveBeenCalledOnce();
});

it('rejects a later unapproved edit to the same approved path', async () => {
  change('approved', `change\n\n${trailer}`);
  change('unapproved', 'second change');
  const source = gitOwnershipCommits(repository, base);
  const pushed = squash(`Squashed change (#1478)\n\n${trailer}`);
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve(source)))
    .rejects.toThrow('original PR commits fail audit');
});

it('does not borrow a trailer from a source commit that did not change that path', async () => {
  const pushed = [{ sha: 'squash', message: trailer, paths: ['frozen.txt'] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, manifest, () => Promise.resolve([
    { sha: 'unrelated', message: trailer, paths: ['ordinary.txt'] },
  ]))).rejects.toThrow('do not authorize final frozen paths');
});

it('accepts a strict direct push with no association', async () => {
  change('approved', `change\n\n${trailer}`);
  const commits = gitOwnershipCommits(repository, base);
  expect(await ownershipCommitsForEvent('push', () => commits, manifest, () => Promise.resolve([]))).toBe(commits);
});

it('preserves newline, tab, space, and rename old/new paths through local NUL-delimited ingress', () => {
  const specialPaths = ['line\nbreak.txt', 'tab\tpath.txt', 'space path.txt'];
  for (const path of specialPaths) writeFileSync(join(repository, path), path);
  git('add', '--', ...specialPaths);
  git('commit', '-m', 'special paths');
  const specialHead = git('rev-parse', 'HEAD');
  expect(gitOwnershipCommits(repository, base, specialHead)[0]?.paths).toEqual(expect.arrayContaining(specialPaths));

  const oldPath = 'rename old\n\t path.txt';
  const newPath = 'rename new\n\t path.txt';
  writeFileSync(join(repository, oldPath), 'rename');
  git('add', '--', oldPath);
  git('commit', '-m', 'rename base');
  const renameBase = git('rev-parse', 'HEAD');
  git('mv', '--', oldPath, newPath);
  git('commit', '-m', 'rename special path');
  expect(gitOwnershipCommits(repository, renameBase)[0]?.paths).toEqual(expect.arrayContaining([oldPath, newPath]));
});

it('rejects a wrapped direct push with no association', async () => {
  change('wrapped', `change\n\n${earlyFoldWrappedTrailer}`);
  const commits = gitOwnershipCommits(repository, base);
  await expect(ownershipCommitsForEvent('push', () => commits, manifest, () => Promise.resolve([])))
    .rejects.toThrow('cannot audit direct ownership push');
});

it.each(['pull_request', undefined])('does not recover evidence for %s', async (event) => {
  change('unapproved', 'change');
  const commits = gitOwnershipCommits(repository, base);
  const recover = vi.fn(() => Promise.resolve([]));
  expect(await ownershipCommitsForEvent(event, () => commits, manifest, recover)).toBe(commits);
  expect(recover).not.toHaveBeenCalled();
});

it('does not request GitHub evidence for ordinary paths', async () => {
  const commits = [{ sha: 'ordinary', message: 'ordinary', paths: ['ordinary.txt'] }];
  const recover = vi.fn(() => Promise.reject(new Error('offline')));
  expect(await ownershipCommitsForEvent('push', () => commits, manifest, recover)).toBe(commits);
  expect(recover).not.toHaveBeenCalled();
});

it('fails closed when association evidence cannot be loaded, even with a complete durable trailer', async () => {
  change('approved', `change\n\n${trailer}`);
  const commits = gitOwnershipCommits(repository, base);
  await expect(ownershipCommitsForEvent('push', () => commits, manifest, () => Promise.reject(new Error('API unavailable'))))
    .rejects.toThrow('API unavailable');
});

it('preserves multiple adjacent source trailers with their exact 72-column wraps', async () => {
  const first = 'OWNERSHIP-CHANGE: frozen.txt — first approved reason that reaches the deterministic wrap boundary exactly (#1478)';
  const firstWrapped = 'OWNERSHIP-CHANGE: frozen.txt — first approved reason that reaches the\ndeterministic wrap boundary exactly (#1478)';
  const second = 'OWNERSHIP-CHANGE: second.txt — second approved reason with another deterministic wrap boundary (#1478)';
  const secondWrapped = 'OWNERSHIP-CHANGE: second.txt — second approved reason with another\ndeterministic wrap boundary (#1478)';
  const entries = [...manifest, { ...manifest[0], path: 'second.txt' }];
  const pushed = [{
    sha: 'squash',
    message: `${firstWrapped}\n${secondWrapped}`,
    paths: ['frozen.txt', 'second.txt'],
  }];
  const source = [{ sha: 'source', message: `${first}\n${second}`, paths: ['frozen.txt', 'second.txt'] }];
  expect(await ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve(source))).toBe(pushed);
});

it.each([
  'OWNERSHIP-CHANGE:\n\nfrozen.txt — approved fixture change (#1478)',
  'OWNERSHIP-CHANGE: frozen.txt\n\n— approved fixture change (#1478)',
  'OWNERSHIP-CHANGE: frozen.txt — approved fixture change\n\n(#1478)',
])('rejects a blank line at a soft-wrap boundary: %s', async (message) => {
  await expect(syntheticAudit(message)).rejects.toThrow('does not preserve source trailers');
});

it('rejects an arbitrary non-72-column newline between canonical words', async () => {
  const message = 'OWNERSHIP-CHANGE: frozen.txt — approved\nfixture change (#1478)';
  await expect(syntheticAudit(message)).rejects.toThrow('does not preserve source trailers');
});

it.each([
  '* OWNERSHIP-CHANGE: frozen.txt — approved fixture change (#1478)',
  'OWNERSHIP-CHANGE: frozen.txt — approved fixture extra change (#1478)',
])('rejects Markdown prefixes and extra tokens: %s', async (message) => {
  await expect(syntheticAudit(message)).rejects.toThrow('does not preserve source trailers');
});

it('does not absorb an ordinary short body line into a source-controlled reason', async () => {
  const source = 'OWNERSHIP-CHANGE: frozen.txt — approved body fixture change (#1478)';
  const durable = 'OWNERSHIP-CHANGE: frozen.txt — approved\nbody\nfixture change (#1478)';
  await expect(syntheticAudit(durable, source)).rejects.toThrow('does not preserve source trailers');
});

it('does not cross into an adjacent OWNERSHIP-CHANGE record supplied by the source reason', async () => {
  const source = 'OWNERSHIP-CHANGE: frozen.txt — approved OWNERSHIP-CHANGE: other.txt — other (#1478)';
  const durable = 'OWNERSHIP-CHANGE: frozen.txt — approved\nOWNERSHIP-CHANGE: other.txt — other (#1478)';
  await expect(syntheticAudit(durable, source)).rejects.toThrow('original PR commits fail audit');
});

it.each([
  'OWNERSHIP-CHANGE: other.txt — approved fixture change (#1478)',
  'OWNERSHIP-CHANGE: frozen.txt — different fixture change (#1478)',
  'OWNERSHIP-CHANGE: frozen.txt — approved fixture change (#1772)',
])('rejects a durable path, reason, or issue mismatch: %s', async (message) => {
  await expect(syntheticAudit(message)).rejects.toThrow('does not preserve source trailers');
});

it('rejects a soft-wrapped trailer in an original source commit', async () => {
  await expect(syntheticAudit(earlyFoldWrappedTrailer, earlyFoldWrappedTrailer)).rejects.toThrow('original PR commits fail audit');
});

it('rejects U+FFFD in direct, source, and final authorization paths', async () => {
  const entries = [{ path: 'root', type: 'directory' as const, owner: 'fixture', readonly: true }];
  const invalid = 'root/bad\uFFFD.ts';
  const canonical = 'root/final.ts';
  const invalidTrailer = `OWNERSHIP-CHANGE: ${invalid} — approved fixture (#1478)`;
  const canonicalTrailer = `OWNERSHIP-CHANGE: ${canonical} — approved fixture (#1478)`;
  const direct = [{ sha: 'direct', message: invalidTrailer, paths: [invalid] }];
  await expect(ownershipCommitsForEvent('push', () => direct, entries, () => Promise.resolve([])))
    .rejects.toThrow('non-canonical changed path');

  const final = [{ sha: 'squash', message: invalidTrailer, paths: [invalid] }];
  await expect(ownershipCommitsForEvent('push', () => final, entries, () => Promise.resolve([{
    sha: 'source', message: invalidTrailer, paths: [invalid],
  }]))).rejects.toThrow('non-canonical changed path');

  const pushed = [{ sha: 'squash', message: canonicalTrailer, paths: [canonical] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve([{
    sha: 'source', message: invalidTrailer, paths: [invalid],
  }]))).rejects.toThrow('original PR commits fail audit');
});

it.each([
  { canonical: 'frozen.txt', raw: './frozen.txt' },
  { canonical: 'dir/frozen.txt', raw: 'dir\\frozen.txt' },
  { canonical: 'frozen.txt', raw: 'frozen.txt/' },
])('rejects non-canonical $raw in direct and associated push audits', async ({ canonical, raw }) => {
  const entries = [{ path: canonical, type: 'file' as const, owner: 'fixture', readonly: true }];
  const rawTrailer = `OWNERSHIP-CHANGE: ${raw} — approved fixture (#1478)`;
  const pushed = [{ sha: 'push', message: rawTrailer, paths: [canonical] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve([])))
    .rejects.toThrow('cannot audit direct ownership push');
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve([{
    sha: 'source', message: rawTrailer, paths: [canonical],
  }]))).rejects.toThrow('original PR commits fail audit');
});

it.each([
  { canonical: 'frozen.txt', raw: './frozen.txt' },
  { canonical: 'dir/frozen.txt', raw: 'dir\\frozen.txt' },
  { canonical: 'frozen.txt', raw: 'frozen.txt/' },
])('does not bind non-canonical source changed path $raw to final $canonical', async ({ canonical, raw }) => {
  const entries = [{ path: canonical, type: 'file' as const, owner: 'fixture', readonly: true }];
  const canonicalTrailer = `OWNERSHIP-CHANGE: ${canonical} — approved fixture (#1478)`;
  const pushed = [{ sha: 'squash', message: canonicalTrailer, paths: [canonical] }];
  const source = [{ sha: 'source', message: canonicalTrailer, paths: [raw] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve(source)))
    .rejects.toThrow('non-canonical changed path');
});

it.each([
  { canonical: 'frozen.txt', raw: './frozen.txt' },
  { canonical: 'dir/frozen.txt', raw: 'dir\\frozen.txt' },
  { canonical: 'frozen.txt', raw: 'frozen.txt/' },
])('rejects non-canonical final changed path $raw for direct and associated pushes', async ({ canonical, raw }) => {
  const entries = [{ path: canonical, type: 'file' as const, owner: 'fixture', readonly: true }];
  const canonicalTrailer = `OWNERSHIP-CHANGE: ${canonical} — approved fixture (#1478)`;
  const pushed = [{ sha: 'push', message: canonicalTrailer, paths: [raw] }];
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve([])))
    .rejects.toThrow('non-canonical changed path');
  await expect(ownershipCommitsForEvent('push', () => pushed, entries, () => Promise.resolve([{
    sha: 'source', message: canonicalTrailer, paths: [canonical],
  }]))).rejects.toThrow('non-canonical changed path');
});
