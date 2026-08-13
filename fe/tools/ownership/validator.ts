import { execFileSync } from 'node:child_process';
import { posix } from 'node:path';

export interface OwnershipEntry {
  path: string;
  type: 'file' | 'directory';
  owner: string;
  readonly?: boolean;
}

export interface OwnershipCommit { sha: string; message: string; paths: readonly string[] }
export interface OwnershipViolation { rule: string; message: string }
export const OWNERSHIP_RULES = Object.freeze([
  'entry-shape', 'exactly-one-owner', 'coverage', 'readonly-change-trailer',
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

function clean(path: string): string {
  return path.replaceAll('\\', '/').replace(/^\.\//, '').replace(/\/$/, '');
}

function validPath(path: string): boolean {
  const normalized = clean(path);
  return normalized !== '' && !normalized.startsWith('/') && !normalized.split('/').includes('..')
    && !['*', '?', '[', ']'].some((character) => normalized.includes(character));
}

function entryMatches(entry: OwnershipEntry, file: string): boolean {
  const target = clean(entry.path);
  const candidate = clean(file);
  return entry.type === 'file' ? candidate === target : candidate === target || candidate.startsWith(`${target}/`);
}

function overlap(left: OwnershipEntry, right: OwnershipEntry): boolean {
  if (clean(left.path) === clean(right.path)) return true;
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
  for (const file of existingFiles.map(clean).sort()) {
    const count = validEntries.filter((entry) => entryMatches(entry, file)).length;
    if (count !== 1) violations.push({ rule: 'coverage', message: `${file} has ${count} owners` });
  }
  for (const commit of commits) {
    const approved = new Set(Array.from(commit.message.matchAll(/^OWNERSHIP-CHANGE:\s+(\S+)\s+—\s+\S.+\s+\(#\d+\)$/gm), (match) => clean(match[1])));
    for (const path of commit.paths.map(clean).sort()) {
      if (!validEntries.some((entry) => entry.readonly === true && entryMatches(entry, path))) continue;
      if (!approved.has(path)) violations.push({
        rule: 'readonly-change-trailer',
        message: `${commit.sha} changes frozen ${path} without an OWNERSHIP-CHANGE trailer`,
      });
    }
  }
  return violations;
}

export function repositoryFiles(repoRoot: string, trackedFiles?: readonly string[]): string[] {
  const roots = ['fe/core', 'fe/mock', 'fe/web', 'fe/tools'];
  const controls: readonly string[] = OWNERSHIP_CONTROL_FILES;
  /*
    'fe/.dependency-cruiser.cjs', 'fe/eslint.config.js', 'fe/module-file-inventory.yaml',
    'fe/ownership-manifest.d.mts', 'fe/ownership-manifest.mjs', 'fe/package.json',
    'fe/package-lock.json', 'fe/stylelint.config.js', 'fe/tsconfig.app.json',
    'fe/tsconfig.core.json', 'fe/tsconfig.json', 'fe/tsconfig.node.json',
    'fe/vite.config.ts', 'fe/vitest.config.ts',
  ]; */
  const files = trackedFiles ?? execFileSync('git', ['ls-files', '--', ...roots, ...controls], {
    cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).split(/\r?\n/).filter(Boolean);
  return files.map((path) => posix.normalize(clean(path)))
    .filter((path) => controls.includes(path)
      || roots.some((directory) => path === directory || path.startsWith(`${directory}/`)))
    .sort();
}

export function gitOwnershipCommits(repoRoot: string, baseSha: string, headRef = 'HEAD'): OwnershipCommit[] {
  const hashes = execFileSync('git', ['log', '--no-merges', '--format=%H', `${baseSha}..${headRef}`, '--'], {
    cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).split(/\r?\n/).filter(Boolean);
  return hashes.map((sha) => ({
    sha,
    message: execFileSync('git', ['log', '-1', '--format=%B', sha], { cwd: repoRoot, encoding: 'utf8' }),
    paths: execFileSync('git', ['diff-tree', '--root', '--no-commit-id', '--name-only', '-r', sha, '--'], {
      cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    }).split(/\r?\n/).filter(Boolean),
  }));
}

export function ownershipCommitsForEvent(
  eventName: string | undefined,
  load: () => readonly OwnershipCommit[],
): readonly OwnershipCommit[] {
  return eventName === 'push' ? [] : load();
}

export function resolveOwnershipBase(repoRoot: string, injectedBase = process.env.OWNERSHIP_BASE_SHA ?? '', headRef = 'HEAD'): string {
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
