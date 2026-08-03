import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { posix, relative, resolve } from 'node:path';

export interface OwnershipEntry {
  path: string;
  type: 'file' | 'directory';
  owner: string;
  readonly?: boolean;
}

export interface ChangeRequest { path: string; reason: string; issue: string; base: string }
export interface OwnershipViolation { rule: string; message: string }
export const OWNERSHIP_RULES = Object.freeze([
  'entry-shape', 'change-request-shape', 'exactly-one-owner', 'coverage', 'stale-change-request', 'readonly-change-request',
] as const);
export const OWNERSHIP_YAML_FIELDS = Object.freeze([
  'entry.path', 'entry.type', 'entry.owner', 'entry.readonly',
  'changeRequest.path', 'changeRequest.reason', 'changeRequest.issue', 'changeRequest.base',
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

function filesUnder(root: string): string[] {
  const result: string[] = [];
  if (!existsSync(root)) return result;
  const visit = (directory: string): void => {
    for (const item of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, item.name);
      if (item.isDirectory()) visit(path);
      else if (item.isFile()) result.push(path);
    }
  };
  visit(root);
  return result;
}

export function validateOwnership(
  entries: readonly unknown[],
  existingFiles: readonly string[],
  changedPaths: readonly string[] = [],
  changeRequests: readonly unknown[] = [],
  changeBase = '',
): OwnershipViolation[] {
  const violations: OwnershipViolation[] = [];
  const validEntries: OwnershipEntry[] = [];
  const validRequests: ChangeRequest[] = [];
  for (const [index, request] of changeRequests.entries()) {
    const candidate = request && typeof request === 'object' && !Array.isArray(request)
      ? request as Record<string, unknown> : {};
    if (typeof candidate.path !== 'string' || !validPath(candidate.path)
      || typeof candidate.reason !== 'string' || candidate.reason.trim() === ''
      || typeof candidate.issue !== 'string' || candidate.issue.trim() === ''
      || typeof candidate.base !== 'string' || !/^[0-9a-f]{40}$/.test(candidate.base)) {
      violations.push({ rule: 'change-request-shape', message: `invalid change request ${index + 1}` });
      continue;
    }
    validRequests.push(candidate as unknown as ChangeRequest);
  }
  const changed = new Set(changedPaths.map(clean));
  for (const request of validRequests) {
    if (request.base !== changeBase || !changed.has(clean(request.path))) {
      violations.push({ rule: 'stale-change-request', message: `${request.path} has a consumed or mismatched change request` });
    }
  }
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
  for (const path of changedPaths.map(clean).sort()) {
    const frozen = validEntries.filter((entry) => entry.readonly === true && entryMatches(entry, path));
    if (frozen.length === 0) continue;
    const approved = validRequests.some((request) => path === clean(request.path) && request.base === changeBase);
    if (!approved) violations.push({ rule: 'readonly-change-request', message: `${path} changed without a change request` });
  }
  return violations;
}

export function repositoryFiles(repoRoot: string): string[] {
  return ['fe/core', 'fe/mock', 'fe/web', 'fe/tools'].flatMap((directory) => filesUnder(resolve(repoRoot, directory)))
    .map((path) => posix.normalize(relative(repoRoot, path).replaceAll('\\', '/')));
}

export function gitChangedPaths(repoRoot: string, baseRef = 'origin/main', headRef = 'HEAD'): string[] {
  const mergeBase = gitMergeBase(repoRoot, baseRef, headRef);
  return execFileSync('git', ['diff', '--name-only', mergeBase, headRef, '--'], { cwd: repoRoot, encoding: 'utf8' })
    .split(/\r?\n/).filter(Boolean);
}

export function gitMergeBase(repoRoot: string, baseRef = 'origin/main', headRef = 'HEAD'): string {
  try {
    return execFileSync('git', ['merge-base', baseRef, headRef], {
      cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    }).trim();
  } catch {
    throw new Error(`cannot resolve ownership audit base ref ${baseRef}; run: git fetch origin main`);
  }
}

export function auditRepositoryOwnership(repoRoot: string, entries: readonly OwnershipEntry[], requests: readonly ChangeRequest[]): OwnershipViolation[] {
  return validateOwnership(entries, repositoryFiles(repoRoot), gitChangedPaths(repoRoot), requests, gitMergeBase(repoRoot));
}
