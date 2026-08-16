import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

/**
 * Executable pin for `run.mjs --plan`: planning is a READ of the worktree.
 * Today that holds only because mkdtempSync and every `git apply` sit textually after
 * `process.exit(0)`; hoisting any of them would silently make the plan job mutate the tree.
 */

const feRoot = resolve(import.meta.dirname, '../..');

/** @param {unknown} condition @param {string} message */
function assert(condition, message) {
  if (!condition) throw new Error(`plan purity: ${message}`);
}

function temporaryDirectoryNames() {
  return new Set(readdirSync(tmpdir()).filter((name) => name.startsWith('neige-mutation-')));
}

const before = temporaryDirectoryNames();
const plan = spawnSync(process.execPath, ['tools/mutation/run.mjs', '--plan'], {
  cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
});
const after = temporaryDirectoryNames();

assert(plan.status === 0, `--plan exited ${plan.status}: ${plan.stderr}`);
const lines = plan.stdout.split('\n').filter((line) => line !== '');
assert(lines.length === 1, `--plan wrote ${lines.length} stdout lines, expected exactly one JSON line`);
const parsed = JSON.parse(lines[0]);
assert(Object.keys(parsed).sort().join(',') === 'clamped,selected,shards,total',
  `--plan JSON keys are ${Object.keys(parsed).sort().join(',')}`);
assert(Array.isArray(parsed.shards) && parsed.shards.length === parsed.total, '--plan shards do not match total');
assert(typeof parsed.selected === 'number' && typeof parsed.clamped === 'boolean', '--plan selected/clamped have wrong types');

const status = spawnSync('git', ['status', '--porcelain'], { cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
assert(status.status === 0, `git status exited ${status.status}`);
assert(status.stdout.trim() === '', `--plan left the worktree dirty:\n${status.stdout}`);

const created = [...after].filter((name) => !before.has(name));
assert(created.length === 0, `--plan created temp directories: ${created.join(', ')}`);

console.log(`mutation plan purity: clean worktree, no temp dir, plan ${lines[0]}`);
