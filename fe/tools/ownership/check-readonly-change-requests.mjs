import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const validatorPath = './validator.ts';
const { gitOwnershipCommits, repositoryFiles, resolveOwnershipBase, validateOwnership } = await import(validatorPath);
const { auditStyleRepository } = await import('../styles/repository-check.mjs');
const { ownershipManifest } = await import('../../ownership-manifest.mjs');

const repository = mkdtempSync(join(tmpdir(), 'ownership-check-'));

/** @param {...string} args */
function git(...args) {
  return execFileSync('git', args, { cwd: repository, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

try {
  const feRoot = join(import.meta.dirname, '../..');
  const base = resolveOwnershipBase(join(feRoot, '..'));
  const commits = process.env.OWNERSHIP_EVENT_NAME === 'push'
    ? []
    : gitOwnershipCommits(join(feRoot, '..'), base);
  const repositoryViolations = validateOwnership(
    ownershipManifest, repositoryFiles(join(feRoot, '..')), commits,
  );
  if (repositoryViolations.length) {
    throw new Error(`repository ownership audit failed:\n${repositoryViolations
      .map(/** @param {{message: string}} violation */ (violation) => violation.message).join('\n')}`);
  }
  console.log(`ownership manifest: ${ownershipManifest.length} entries, complete coverage, no overlaps`);
  const styleViolations = auditStyleRepository(join(import.meta.dirname, '../..'));
  if (styleViolations.length) throw new Error(`style repository audit failed:\n${styleViolations.join('\n')}`);
  console.log('style manifests, CSS Module layers, and data-* attributes: valid');
  git('init', '--initial-branch=main');
  git('config', 'user.email', 'ownership-check@example.invalid');
  git('config', 'user.name', 'Ownership Checker');
  writeFileSync(join(repository, 'frozen.txt'), 'before\n');
  git('add', 'frozen.txt');
  git('commit', '-m', 'base');
  git('branch', 'origin/main');
  writeFileSync(join(repository, 'frozen.txt'), 'after\n');
  git('add', 'frozen.txt');
  git('commit', '-m', 'change');

  const fixtureBase = resolveOwnershipBase(repository, '');
  const violations = validateOwnership(
    [{ path: 'frozen.txt', type: 'file', owner: 'fixture', readonly: true }], [], gitOwnershipCommits(repository, fixtureBase),
  );
  if (violations.length !== 1 || violations[0]?.rule !== 'readonly-change-trailer') {
    throw new Error('ownership checker failed to reject a readonly change without a trailer');
  }

  const headParent = git('rev-parse', 'HEAD~1').trim();
  if (resolveOwnershipBase(repository, '0'.repeat(40)) !== headParent) throw new Error('zero base did not fall back to HEAD~1');
  if (resolveOwnershipBase(repository, 'f'.repeat(40)) !== headParent) throw new Error('missing injected base did not fall back to HEAD~1');

  git('branch', '-D', 'origin/main');
  try {
    resolveOwnershipBase(repository, '');
    throw new Error('ownership checker silently accepted a missing base ref');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('git fetch origin main')) throw error;
  }
  console.log(`ownership readonly trailer alarm: ${violations[0].message}`);
  console.log('ownership injected-base fallback: zero or unavailable SHA resolves to HEAD~1');
  console.log('ownership missing-ref check: fail-closed with git fetch origin main guidance');
} finally {
  rmSync(repository, { recursive: true, force: true });
}
