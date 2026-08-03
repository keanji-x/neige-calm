import { execFileSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { posix, relative, resolve } from 'node:path';

export interface OwnershipEntry {
  path: string;
  type: 'file' | 'directory';
  owner: string;
  readonly?: boolean;
}

export interface ChangeRequest { path: string; reason: string }
export interface OwnershipViolation { rule: string; message: string }

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
  entries: readonly OwnershipEntry[],
  existingFiles: readonly string[],
  changedPaths: readonly string[] = [],
  changeRequests: readonly ChangeRequest[] = [],
): OwnershipViolation[] {
  const violations: OwnershipViolation[] = [];
  for (const [index, entry] of entries.entries()) {
    if (!validPath(entry.path) || !['file', 'directory'].includes(entry.type) || entry.owner.trim() === '') {
      violations.push({ rule: 'entry-shape', message: `invalid entry ${index + 1}: ${entry.path}` });
    }
  }
  for (let left = 0; left < entries.length; left += 1) {
    for (let right = left + 1; right < entries.length; right += 1) {
      if (overlap(entries[left], entries[right])) {
        violations.push({ rule: 'exactly-one-owner', message: `${entries[left].path} overlaps ${entries[right].path}` });
      }
    }
  }
  for (const file of existingFiles.map(clean).sort()) {
    const count = entries.filter((entry) => entryMatches(entry, file)).length;
    if (count !== 1) violations.push({ rule: 'coverage', message: `${file} has ${count} owners` });
  }
  for (const path of changedPaths.map(clean).sort()) {
    const frozen = entries.filter((entry) => entry.readonly === true && entryMatches(entry, path));
    if (frozen.length === 0) continue;
    const approved = changeRequests.some((request) => request.reason.trim() !== '' && (path === clean(request.path) || path.startsWith(`${clean(request.path)}/`)));
    if (!approved) violations.push({ rule: 'readonly-change-request', message: `${path} changed without a change request` });
  }
  return violations;
}

export function repositoryFiles(repoRoot: string): string[] {
  return ['fe/core', 'fe/web/src'].flatMap((directory) => filesUnder(resolve(repoRoot, directory)))
    .map((path) => posix.normalize(relative(repoRoot, path).replaceAll('\\', '/')));
}

export function gitChangedPaths(repoRoot: string, baseRef = 'origin/main', headRef = 'HEAD'): string[] {
  const mergeBase = execFileSync('git', ['merge-base', baseRef, headRef], { cwd: repoRoot, encoding: 'utf8' }).trim();
  return execFileSync('git', ['diff', '--name-only', mergeBase, headRef, '--'], { cwd: repoRoot, encoding: 'utf8' })
    .split(/\r?\n/).filter(Boolean);
}

export function auditRepositoryOwnership(repoRoot: string, entries: readonly OwnershipEntry[], requests: readonly ChangeRequest[]): OwnershipViolation[] {
  return validateOwnership(entries, repositoryFiles(repoRoot), gitChangedPaths(repoRoot), requests);
}
