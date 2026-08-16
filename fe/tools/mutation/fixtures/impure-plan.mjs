/**
 * SINGLE-VIOLATION FIXTURE for plan-purity.mjs — a deliberately IMPURE stand-in for
 * `run.mjs --plan`, and the only thing that proves the purity pin can go RED.
 *
 * Everything about it is a faithful plan except the one violation: it exits 0 and prints exactly
 * one JSON line with the plan keys, so the end-state assertions all pass. The violation is that it
 * TOUCHES THE FILESYSTEM WHILE PLANNING — a temp dir under TMPDIR plus a write into the worktree —
 * and then cleans both up before exiting, which is precisely the shape that defeated the old
 * post-state-only oracle (review round 2). Only the sealed-TMPDIR run can see it.
 *
 * Do not "fix" this file. It is the negative control; if it ever passes plan-purity.mjs, the pin is
 * dead. It cleans up after itself so the worktree stays clean even when the harness stops early.
 */
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

const feRoot = resolve(import.meta.dirname, '../../..');
// os.tmpdir() re-reads TMPDIR on every call, so a sealed TMPDIR makes this throw EACCES → exit 1.
const temporary = mkdtempSync(resolve(tmpdir(), 'impure-plan-'));
const scratch = resolve(feRoot, 'tools/mutation/fixtures/.impure-plan-scratch');
try {
  writeFileSync(scratch, 'transient worktree write; deleted below, which is exactly the point\n');
} finally {
  rmSync(scratch, { force: true });
  rmSync(temporary, { recursive: true, force: true });
}
console.log(JSON.stringify({ selected: 0, total: 1, shards: [1], clamped: false }));
