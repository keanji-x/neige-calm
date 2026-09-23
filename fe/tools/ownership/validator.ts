import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { posix, resolve } from 'node:path';

export interface OwnershipEntry {
  path: string;
  type: 'file' | 'directory';
  owner: string;
  readonly?: boolean;
}

export interface OwnershipCommit { sha: string; message: string; paths: readonly string[] }
export interface OwnershipViolation { rule: string; message: string }
export const OWNERSHIP_RULES = Object.freeze([
  'entry-shape', 'exactly-one-owner', 'coverage', 'readonly-change-trailer', 'readonly-change-pr-body',
] as const);
export const OWNERSHIP_YAML_FIELDS = Object.freeze([
  'entry.path', 'entry.type', 'entry.owner', 'entry.readonly',
] as const);
export const OWNERSHIP_CONTROL_FILES = Object.freeze([
  'fe/.dependency-cruiser.cjs', 'fe/eslint.config.js', 'fe/module-file-inventory.yaml',
  'fe/ownership-manifest.d.mts', 'fe/ownership-manifest.mjs', 'fe/package.json', 'fe/package-lock.json',
  'fe/stylelint.config.js', 'fe/tsconfig.app.json', 'fe/tsconfig.core.json', 'fe/tsconfig.json',
  'fe/tsconfig.node.json', 'fe/vite.config.ts', 'fe/vitest.config.ts',
] as const);

interface OwnershipTrailer { text: string; path: string }

export function canonicalRepoRelativePath(path: string): boolean {
  return path !== '' && !path.includes('\uFFFD') && !path.startsWith('/') && !path.includes('\\') && path === posix.normalize(path)
    && path.split('/').every((segment) => segment !== '' && segment !== '.' && segment !== '..')
    && !['*', '?', '[', ']'].some((character) => path.includes(character));
}

function ownershipTrailers(message: string): OwnershipTrailer[] {
  return message.split(/\r?\n/).flatMap((line) => {
    const match = /^OWNERSHIP-CHANGE: (\S+) — (\S+(?: \S+)*) \(#\d+\)$/.exec(line);
    if (!match || !canonicalRepoRelativePath(match[1])
      || match[2].split(' ').includes('OWNERSHIP-CHANGE:')) return [];
    return [{ text: line, path: match[1] }];
  });
}

function frozenChangedPaths(entries: readonly OwnershipEntry[], commit: OwnershipCommit): string[] {
  return commit.paths.filter((path) => entries.some((entry) => entry.readonly === true
    && entryMatches(entry, path)));
}

function codePointLength(value: string): number {
  return [...value].length;
}

function githubWrapTrailer(trailer: string): string {
  const lines: string[] = [];
  for (const word of trailer.split(' ')) {
    const current = lines.at(-1);
    if (current === undefined || codePointLength(current) + 1 + codePointLength(word) > 72) lines.push(word);
    else lines[lines.length - 1] = `${current} ${word}`;
  }
  return lines.join('\n');
}

function hasPhysicalLines(message: string, representation: string): boolean {
  const lines = message.replaceAll('\r\n', '\n').split('\n');
  const expected = representation.split('\n');
  return lines.some((_, start) => expected.every((line, offset) => lines[start + offset] === line));
}

function durableMessagePreserves(message: string, trailer: string): boolean {
  return hasPhysicalLines(message, trailer) || hasPhysicalLines(message, githubWrapTrailer(trailer));
}

function validPath(path: string): boolean {
  return canonicalRepoRelativePath(path);
}

function entryMatches(entry: OwnershipEntry, file: string): boolean {
  return entry.type === 'file' ? file === entry.path : file === entry.path || file.startsWith(`${entry.path}/`);
}

function overlap(left: OwnershipEntry, right: OwnershipEntry): boolean {
  if (left.path === right.path) return true;
  if (left.type === 'directory' && entryMatches(left, right.path)) return true;
  return right.type === 'directory' && entryMatches(right, left.path);
}

export function validateOwnership(
  entries: readonly unknown[],
  existingFiles: readonly string[],
  commits: readonly OwnershipCommit[] = [],
): OwnershipViolation[] {
  const violations: OwnershipViolation[] = [];
  const validEntries: OwnershipEntry[] = [];
  for (const [index, entry] of entries.entries()) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      violations.push({ rule: 'entry-shape', message: `invalid entry ${index + 1}: ${String(entry)}` });
      continue;
    }
    const candidate = entry as Record<string, unknown>;
    const readonlyValid = typeof candidate.readonly === 'boolean';
    if (typeof candidate.path !== 'string' || typeof candidate.type !== 'string'
      || typeof candidate.owner !== 'string' || !validPath(candidate.path)
      || !['file', 'directory'].includes(candidate.type) || candidate.owner.trim() === '' || !readonlyValid) {
      violations.push({ rule: 'entry-shape', message: `invalid entry ${index + 1}: ${String(candidate.path)}` });
      if (typeof candidate.path !== 'string' || typeof candidate.type !== 'string'
        || typeof candidate.owner !== 'string' || !validPath(candidate.path)
        || !['file', 'directory'].includes(candidate.type) || candidate.owner.trim() === '') continue;
    }
    validEntries.push({
      path: candidate.path,
      type: candidate.type as OwnershipEntry['type'],
      owner: candidate.owner,
      readonly: readonlyValid ? candidate.readonly as boolean : true,
    });
  }
  for (let left = 0; left < validEntries.length; left += 1) {
    for (let right = left + 1; right < validEntries.length; right += 1) {
      if (overlap(validEntries[left], validEntries[right])) {
        violations.push({ rule: 'exactly-one-owner', message: `${validEntries[left].path} overlaps ${validEntries[right].path}` });
      }
    }
  }
  for (const file of [...existingFiles].sort()) {
    if (!canonicalRepoRelativePath(file)) {
      violations.push({ rule: 'coverage', message: `non-canonical repository path: ${file}` });
      continue;
    }
    const count = validEntries.filter((entry) => entryMatches(entry, file)).length;
    if (count !== 1) violations.push({ rule: 'coverage', message: `${file} has ${count} owners` });
  }
  for (const commit of commits) {
    const approved = new Set(ownershipTrailers(commit.message).map(({ path }) => path));
    for (const path of [...new Set(commit.paths)].sort()) {
      if (!canonicalRepoRelativePath(path)) {
        violations.push({
          rule: 'readonly-change-trailer',
          message: `${commit.sha} has non-canonical changed path ${path}`,
        });
        continue;
      }
      if (!validEntries.some((entry) => entry.readonly === true && entryMatches(entry, path))) continue;
      if (!approved.has(path)) violations.push({
        rule: 'readonly-change-trailer',
        message: `${commit.sha} changes frozen ${path} without an OWNERSHIP-CHANGE trailer`,
      });
    }
  }
  return violations;
}

export function validateOwnershipPullRequestBody(
  eventName: string | undefined,
  commits: readonly OwnershipCommit[],
  pullRequestBody: string,
): OwnershipViolation[] {
  if (eventName !== 'pull_request') return [];
  const bodyTrailers = new Set(ownershipTrailers(pullRequestBody).map(({ text }) => text));
  const missing = new Map<string, string>();
  for (const commit of commits) {
    for (const { text: trailer } of ownershipTrailers(commit.message)) {
      if (!bodyTrailers.has(trailer) && !missing.has(trailer)) missing.set(trailer, commit.sha);
    }
  }
  return Array.from(missing, ([trailer, sha]) => ({
    rule: 'readonly-change-pr-body',
    message: `${sha} has ${trailer} but the pull request body does not preserve it for the squash commit`,
  }));
}

export function repositoryFiles(repoRoot: string, trackedFiles?: readonly string[]): string[] {
  const roots = ['fe/core', 'fe/web', 'fe/tools'];
  const controls: readonly string[] = OWNERSHIP_CONTROL_FILES;
  const files = trackedFiles ?? execFileSync('git', ['ls-files', '-z', '--', ...roots, ...controls], {
    cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).split('\0').filter(Boolean);
  const nonCanonical = files.find((path) => !canonicalRepoRelativePath(path));
  if (nonCanonical !== undefined) throw new Error(`non-canonical repository path: ${nonCanonical}`);
  return files.filter((path) => trackedFiles !== undefined || existsSync(resolve(repoRoot, path)))
    .filter((path) => controls.includes(path)
      || roots.some((directory) => path === directory || path.startsWith(`${directory}/`)))
    .sort();
}

export function gitOwnershipCommits(repoRoot: string, baseSha: string, headRef = 'HEAD'): OwnershipCommit[] {
  const range = `${baseSha}..${headRef}`;
  const merges = execFileSync('git', ['log', '--merges', '--format=%H', range, '--'], {
    cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).split(/\r?\n/).filter(Boolean);
  if (merges.length > 0) {
    throw new Error(`cannot audit ownership range ${range}; rebase merge commits before review: ${merges.join(', ')}`);
  }
  const hashes = execFileSync('git', ['log', '--format=%H', range, '--'], {
    cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).split(/\r?\n/).filter(Boolean);
  return hashes.map((sha) => ({
    sha,
    message: execFileSync('git', ['log', '-1', '--format=%B', sha], { cwd: repoRoot, encoding: 'utf8' }),
    paths: execFileSync('git', ['diff-tree', '--root', '--no-commit-id', '--name-only', '-z', '-r', sha, '--'], {
      cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    }).split('\0').filter(Boolean),
  }));
}

export async function ownershipCommitsForEvent(
  eventName: string | undefined,
  load: () => readonly OwnershipCommit[],
  entries: readonly OwnershipEntry[],
  recover: (commit: OwnershipCommit) => Promise<readonly OwnershipCommit[]>,
): Promise<readonly OwnershipCommit[]> {
  const commits = load();
  if (eventName !== 'push') return commits;
  for (const commit of commits) {
    const invalidPaths = commit.paths.filter((path) => !canonicalRepoRelativePath(path));
    if (invalidPaths.length > 0) {
      throw new Error(`cannot audit ownership push ${commit.sha}: ${commit.sha} has non-canonical changed path ${invalidPaths.join(', ')}`);
    }
    const changedFrozenPaths = frozenChangedPaths(entries, commit);
    if (changedFrozenPaths.length === 0) continue;
    const source = await recover(commit);
    if (source.length === 0) {
      const violations = validateOwnership(entries, [], [commit]);
      if (violations.length > 0) {
        throw new Error(`cannot audit direct ownership push ${commit.sha}:\n${violations
          .map(({ message }) => message).join('\n')}`);
      }
      continue;
    }
    const violations = validateOwnership(entries, [], source);
    if (violations.length > 0) {
      throw new Error(`cannot audit ownership squash ${commit.sha}: original PR commits fail audit:\n${violations
        .map(({ message }) => message).join('\n')}`);
    }
    const trailers = new Map(source.flatMap((original) => {
      const changed = new Set(original.paths);
      return ownershipTrailers(original.message).filter(({ path }) => changed.has(path))
        .map((trailer) => [trailer.text, trailer] as const);
    }));
    const authorizedPaths = new Set(Array.from(trailers.values(), ({ path }) => path));
    const unauthorized = [...new Set(changedFrozenPaths)].filter((path) => !authorizedPaths.has(path));
    if (unauthorized.length > 0) {
      throw new Error(`cannot audit ownership squash ${commit.sha}: original PR commits do not authorize final frozen paths: ${unauthorized.join(', ')}`);
    }
    const missing = Array.from(trailers.keys()).filter((trailer) => !durableMessagePreserves(commit.message, trailer));
    if (missing.length > 0) {
      throw new Error(`cannot audit ownership squash ${commit.sha}: final commit message does not preserve source trailers:\n${missing.join('\n')}`);
    }
  }
  return commits;
}

export function resolveOwnershipBase(
  repoRoot: string,
  injectedBase: string,
  headRef = 'HEAD',
  eventName?: string,
  pushForced = false,
): string {
  if (eventName === 'pull_request') {
    if (![injectedBase, headRef].every((sha) => /^[0-9a-f]{40}$/i.test(sha) && !/^0{40}$/.test(sha))) {
      throw new Error('cannot audit ownership pull request: base or head SHA is missing or zero');
    }
    try {
      execFileSync('git', ['cat-file', '-e', `${injectedBase}^{commit}`], { cwd: repoRoot, stdio: 'ignore' });
      execFileSync('git', ['cat-file', '-e', `${headRef}^{commit}`], { cwd: repoRoot, stdio: 'ignore' });
      return execFileSync('git', ['merge-base', injectedBase, headRef], {
        cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
      }).trim();
    } catch {
      throw new Error(`cannot audit ownership pull request range ${injectedBase}..${headRef}; history is unavailable`);
    }
  }
  if (eventName === 'push') {
    if (pushForced) throw new Error('cannot audit ownership for a forced push');
    if (!/^[0-9a-f]{40}$/i.test(injectedBase) || /^0{40}$/.test(injectedBase)) {
      throw new Error('cannot audit ownership push: github.event.before is missing or zero');
    }
    if (!/^[0-9a-f]{40}$/i.test(headRef) || /^0{40}$/.test(headRef)) {
      throw new Error('cannot audit ownership push: github.event.after is missing or zero');
    }
    try {
      execFileSync('git', ['cat-file', '-e', `${injectedBase}^{commit}`], { cwd: repoRoot, stdio: 'ignore' });
      execFileSync('git', ['cat-file', '-e', `${headRef}^{commit}`], { cwd: repoRoot, stdio: 'ignore' });
      execFileSync('git', ['merge-base', '--is-ancestor', injectedBase, headRef], { cwd: repoRoot, stdio: 'ignore' });
    } catch {
      throw new Error(`cannot audit ownership push range ${injectedBase}..${headRef}; history is unavailable or non-linear`);
    }
    return injectedBase;
  }
  if (injectedBase !== '') {
    try {
      if (!/^0{40}$/.test(injectedBase)) {
        execFileSync('git', ['cat-file', '-e', `${injectedBase}^{commit}`], { cwd: repoRoot, stdio: 'ignore' });
        return execFileSync('git', ['merge-base', injectedBase, headRef], {
          cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
        }).trim();
      }
    } catch { /* event base unavailable: use the frozen-vectors fallback below */ }
    try {
      return execFileSync('git', ['rev-parse', `${headRef}~1`], {
        cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
      }).trim();
    } catch {
      throw new Error(`cannot fall back to ownership audit base ${headRef}~1; the repository needs at least two commits`);
    }
  }
  try {
    return execFileSync('git', ['merge-base', 'origin/main', headRef], {
      cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    }).trim();
  } catch {
    throw new Error('cannot resolve ownership audit base ref origin/main; run: git fetch origin main');
  }
}
