import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const validatorPath = './validator.ts';
const { gitChangedPaths, validateOwnership } = await import(validatorPath);
const { auditStyleRepository } = await import('../styles/repository-check.mjs');

const repository = mkdtempSync(join(tmpdir(), 'ownership-check-'));

/** @param {...string} args */
function git(...args) {
  return execFileSync('git', args, { cwd: repository, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

try {
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

  const changed = gitChangedPaths(repository);
  const violations = validateOwnership(
    [{ path: 'frozen.txt', type: 'file', owner: 'fixture', readonly: true }], [], changed, [],
  );
  if (violations.length !== 1 || violations[0]?.rule !== 'readonly-change-request') {
    throw new Error('ownership checker failed to reject a readonly change without a change request');
  }

  git('branch', '-D', 'origin/main');
  try {
    gitChangedPaths(repository);
    throw new Error('ownership checker silently accepted a missing base ref');
  } catch (error) {
    if (!(error instanceof Error) || !error.message.includes('git fetch origin main')) throw error;
  }
  console.log(`ownership readonly alarm: ${violations[0].message}`);
  console.log('ownership missing-ref check: fail-closed with git fetch origin main guidance');
} finally {
  rmSync(repository, { recursive: true, force: true });
}
