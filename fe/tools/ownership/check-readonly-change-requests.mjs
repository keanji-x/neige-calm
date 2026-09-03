import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const validatorPath = './validator.ts';
const { gitOwnershipCommits, ownershipCommitsForEvent, repositoryFiles, resolveOwnershipBase, validateOwnership } = await import(validatorPath);
const { auditStyleRepository } = await import('../styles/repository-check.mjs');
const { ownershipManifest } = await import('../../ownership-manifest.mjs');

const repository = mkdtempSync(join(tmpdir(), 'ownership-check-'));

/** @param {...string} args */
function git(...args) {
  return execFileSync('git', args, { cwd: repository, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

try {
  const feRoot = join(import.meta.dirname, '../..');
  const head = process.env.OWNERSHIP_HEAD_SHA ?? 'HEAD';
  const base = resolveOwnershipBase(
    join(feRoot, '..'),
    process.env.OWNERSHIP_BASE_SHA ?? '',
    head,
    process.env.OWNERSHIP_EVENT_NAME,
    process.env.OWNERSHIP_PUSH_FORCED === 'true',
  );
  const commits = ownershipCommitsForEvent(process.env.OWNERSHIP_EVENT_NAME,
    () => gitOwnershipCommits(join(feRoot, '..'), base, head));
  const repositoryViolations = validateOwnership(
    ownershipManifest, repositoryFiles(join(feRoot, '..')), commits,
  );
  if (repositoryViolations.length) {
    throw new Error(`repository ownership audit failed:\n${repositoryViolations
      .map(/** @param {{message: string}} violation */ (violation) => violation.message).join('\n')}\n`
      + 'if the commit is already on main, run `git fetch origin main`');
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
  try {
    resolveOwnershipBase(repository, '0'.repeat(40));
    throw new Error('ownership checker silently accepted a missing parent');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('repository needs at least two commits')) throw error;
  }
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

  const pushHead = git('rev-parse', 'HEAD').trim();
  const pushBase = resolveOwnershipBase(repository, fixtureBase, pushHead, 'push');
  const pushViolations = validateOwnership(
    [{ path: 'frozen.txt', type: 'file', owner: 'fixture', readonly: true }], [],
    gitOwnershipCommits(repository, pushBase, pushHead),
  );
  if (pushViolations.length !== 1 || pushViolations[0]?.rule !== 'readonly-change-trailer') {
    throw new Error('push fixture silently allowed a readonly change without a trailer');
  }
  git('commit', '--amend', '-m', 'change\n\nOWNERSHIP-CHANGE: frozen.txt — approved fixture change (#1062)');
  const approvedPushHead = git('rev-parse', 'HEAD').trim();
  const approvedPushViolations = validateOwnership(
    [{ path: 'frozen.txt', type: 'file', owner: 'fixture', readonly: true }], [],
    gitOwnershipCommits(repository, resolveOwnershipBase(repository, fixtureBase, approvedPushHead, 'push'), approvedPushHead),
  );
  if (approvedPushViolations.length !== 0) throw new Error('push fixture rejected a valid ownership trailer');

  for (const [label, baseSha, afterSha, forced] of [
    ['zero before', '0'.repeat(40), approvedPushHead, false],
    ['zero after', fixtureBase, '0'.repeat(40), false],
    ['forced push', fixtureBase, approvedPushHead, true],
    ['unavailable history', 'f'.repeat(40), approvedPushHead, false],
  ]) {
    try {
      resolveOwnershipBase(repository, baseSha, afterSha, 'push', forced);
      throw new Error(`ownership checker silently accepted ${label}`);
    } catch (error) {
      if (!(error instanceof Error) || !error.message.includes('cannot audit ownership')) throw error;
    }
  }

  if (resolveOwnershipBase(repository, fixtureBase, approvedPushHead, 'pull_request') !== fixtureBase) {
    throw new Error('pull request range did not resolve through its merge base');
  }
  for (const [baseSha, headSha] of [
    ['0'.repeat(40), approvedPushHead],
    [fixtureBase, '0'.repeat(40)],
    ['f'.repeat(40), approvedPushHead],
  ]) {
    try {
      resolveOwnershipBase(repository, baseSha, headSha, 'pull_request');
      throw new Error('ownership checker silently accepted an invalid pull request range');
    } catch (error) {
      if (!(error instanceof Error) || !error.message.includes('cannot audit ownership pull request')) throw error;
    }
  }

  const headParent = git('rev-parse', 'HEAD~1').trim();
  if (resolveOwnershipBase(repository, '0'.repeat(40)) !== headParent) throw new Error('zero base did not fall back to HEAD~1');
  if (resolveOwnershipBase(repository, 'f'.repeat(40)) !== headParent) throw new Error('missing injected base did not fall back to HEAD~1');

  const featureHead = git('rev-parse', 'HEAD').trim();
  git('checkout', '-b', 'advanced-target', headParent);
  writeFileSync(join(repository, 'target.txt'), 'target advanced\n');
  git('add', 'target.txt');
  git('commit', '-m', 'target advance without ownership trailer');
  const advancedTarget = git('rev-parse', 'HEAD').trim();
  const divergentBase = resolveOwnershipBase(repository, advancedTarget, featureHead);
  if (divergentBase !== headParent) throw new Error('non-ancestor injected base did not resolve through merge-base');
  const featureCommits = gitOwnershipCommits(repository, divergentBase, featureHead);
  if (featureCommits.length !== 1 || featureCommits[0]?.sha !== featureHead) {
    throw new Error('non-ancestor injected base leaked target-branch commits into the ownership range');
  }

  git('checkout', 'main');
  git('merge', '--no-ff', 'advanced-target', '-m', 'merge fixture');
  const mergeHead = git('rev-parse', 'HEAD').trim();
  const explicitLinearCommits = gitOwnershipCommits(repository, fixtureBase, approvedPushHead);
  if (explicitLinearCommits.length !== 1 || explicitLinearCommits[0]?.sha !== approvedPushHead) {
    throw new Error('synthetic merge checkout leaked into an explicit linear pull request range');
  }
  try {
    gitOwnershipCommits(repository, fixtureBase, mergeHead);
    throw new Error('ownership checker silently accepted a merge commit');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('rebase merge commits before review')) throw error;
  }

  git('branch', '-D', 'origin/main');
  try {
    resolveOwnershipBase(repository, '');
    throw new Error('ownership checker silently accepted a missing base ref');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('git fetch origin main')) throw error;
  }
  console.log(`ownership readonly trailer alarm: ${violations[0].message}`);
  console.log(`ownership push readonly negative: RED (${pushViolations[0].message})`);
  console.log('ownership push readonly approved: GREEN');
  console.log('ownership push invalid ranges: fail-closed for zero before/after, forced push, unavailable history');
  console.log('ownership pull request ranges: merge-base resolved, invalid or unavailable history fails closed');
  console.log('ownership injected-base fallback: zero or unavailable SHA resolves to HEAD~1');
  console.log(`ownership non-ancestor base: ${advancedTarget} -> merge-base ${divergentBase}; range contains feature ${featureHead} only`);
  console.log(`ownership merge range: RED (${mergeHead} requires rebase before review)`);
  console.log(`ownership explicit head: ${approvedPushHead} stays linear under synthetic merge checkout ${mergeHead}`);
  console.log('ownership single-commit fallback: fail-closed with a two-commit diagnostic');
  console.log('ownership missing-ref check: fail-closed with git fetch origin main guidance');
} finally {
  rmSync(repository, { recursive: true, force: true });
}
