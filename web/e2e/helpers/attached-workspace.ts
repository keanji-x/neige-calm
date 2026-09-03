// #1147 S3 — real on-disk directories for specs whose SUBJECT is the
// attached-workspace `cwd`.
//
// Since S3, `POST /api/tracks` with an explicit `cwd` (the *attached*
// branch) requires that path to be absolute, to already exist, and to
// sit inside a Git work tree — see
// `crates/calm-server/src/workspace_materialize.rs::validate_attached_workspace`.
// Specs that only need *a* track should omit `cwd` entirely and take the
// kernel-managed branch (see `helpers/reset.ts::createTrackInArea`).
// This module is for the ones that cannot: the legacy `web/` NewTaskForm
// always puts the cwd input's value on the wire, and a handful of specs
// assert on `track.cwd` / `area_folders.path`.
//
// ## Why $HOME
//
// The path has to exist *inside the kernel*, and the kernel is not always
// in the same filesystem as the spec:
//
//   * `chromium e2e` / `fe e2e` run the kernel in the docker stack
//     (`docker-compose.yml`), whose `server` service bind-mounts
//     `${HOME}:${HOME}` — the host's home directory at the *identical*
//     path inside the container. So a directory this module creates is
//     visible to the kernel under the same absolute path, and the spec
//     can type that one path into the form and assert it back off the
//     track row.
//   * `a11y` runs the `replay` binary natively on the runner, so the
//     filesystem is simply shared.
//
// `/tmp` would work in CI only by accident (the workflow's `.env` sets
// `CALM_EXTRA_MOUNT=/tmp`, which mounts the host `/tmp` into the
// container); on a developer box that variable points at the repo drive
// instead and the container gets its own private `/tmp`. `$HOME` is
// mounted by design in every environment, which is why
// `track-create-browse-cwd.spec.ts` already depended on it before this
// change.

import { execFileSync } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';

/** Directories minted by this module, for `cleanupAttachedWorkspaces`. */
const minted: string[] = [];

/**
 * Git environment that would redirect `git init` somewhere other than the
 * directory we name. The kernel scrubs the same family before every git
 * spawn (`workspace_materialize.rs::HOSTILE_GIT_ENV`); we scrub it here so
 * a developer running the suite from inside a repo-manipulating shell
 * gets the same result CI does.
 */
const HOSTILE_GIT_ENV = [
  'GIT_DIR',
  'GIT_WORK_TREE',
  'GIT_COMMON_DIR',
  'GIT_INDEX_FILE',
  'GIT_OBJECT_DIRECTORY',
  'GIT_ALTERNATE_OBJECT_DIRECTORIES',
  'GIT_NAMESPACE',
  'GIT_CEILING_DIRECTORIES',
  'GIT_TEMPLATE_DIR',
  'GIT_CONFIG_GLOBAL',
  'GIT_CONFIG_SYSTEM',
  'GIT_CONFIG',
  'GIT_CONFIG_COUNT',
];

function scrubbedGitEnv(): NodeJS.ProcessEnv {
  const env = { ...process.env };
  for (const key of HOSTILE_GIT_ENV) delete env[key];
  return env;
}

/**
 * An absolute path under `$HOME` for a per-run fixture. `name` should
 * already carry the spec's uniqueness (a timestamp, usually) so parallel
 * or repeated runs never collide on `area_folders.UNIQUE(path)`.
 */
export function attachedWorkspacePath(name: string): string {
  return path.join(homedir(), name);
}

/**
 * Create `dir` (with parents) and make it the root of a Git work tree, so
 * it satisfies `validate_attached_workspace`. Registered for
 * `cleanupAttachedWorkspaces`. Returns `dir` for chaining.
 *
 * `git init` alone is enough: the kernel's check is
 * `git -C <path> rev-parse --show-toplevel`, which succeeds on a fresh
 * repository with no commits. We deliberately do not commit anything —
 * the specs here assert on the track row and the folder claim, not on
 * repository contents.
 */
export function createGitWorkTree(dir: string): string {
  mkdirSync(dir, { recursive: true });
  minted.push(dir);
  execFileSync('git', ['init', '--quiet', dir], {
    stdio: 'ignore',
    env: scrubbedGitEnv(),
  });
  return dir;
}

/**
 * Create a subdirectory *inside* an existing work tree and return its
 * absolute path. `rev-parse --show-toplevel` succeeds anywhere under the
 * work tree, so a descendant is a valid attached workspace too — which is
 * exactly the shape the area-auto-match specs need (an area claims the
 * root, the track attaches a directory beneath it).
 */
export function createWorkTreeSubdir(root: string, ...segments: string[]): string {
  const dir = path.join(root, ...segments);
  mkdirSync(dir, { recursive: true });
  return dir;
}

/**
 * Remove every directory this module minted. Call from `test.afterEach`
 * so the runner's `$HOME` does not accumulate fixtures across runs.
 */
export function cleanupAttachedWorkspaces(): void {
  for (const dir of minted.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
}
