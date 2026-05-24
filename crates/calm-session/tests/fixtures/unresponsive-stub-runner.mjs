#!/usr/bin/env node
// Stub runner for #272 (N1) — `parent_death_orphan::parent_death_kills_chat_runner_group`.
// Models an unresponsive chat runner:
//   * Emits `session_init` so the daemon's chat-mode handshake completes
//     (otherwise the test can't even tell the daemon was healthy before
//     the parent died).
//   * Ignores SIGHUP and SIGPIPE so the EOF-on-stdin-drop happy path
//     doesn't trigger a clean exit. The only way to kill this runner is
//     the daemon's 2 s SIGKILL fallback in `kill_chat_runner_group`.
//   * Drains stdin via readline so a stdin close doesn't even raise.
//   * Sleeps "forever" (10 min) — long enough that a passing test never
//     hits the natural exit, short enough that an aborted CI run cleans
//     itself up.
//
// The companion test SIGKILLs the wrapping sh, expects the daemon to
// receive PR_SET_PDEATHSIG-delivered SIGTERM, drop stdin (no effect on
// this runner), and then within ~2 s SIGHUP-then-SIGKILL the runner's
// pgid. Asserting the runner pid disappears within 5 s proves both N1
// (group kill path exists) and R3 (signal handler armed before any race
// window).

import { createInterface } from 'node:readline';
import process from 'node:process';

process.stdout.write(
  JSON.stringify({
    type: 'session_init',
    session_id: '00000000-0000-0000-0000-000000000000',
    model: 'unresponsive-stub',
    permission_mode: 'default',
    cwd: '',
    version: 'unresponsive-stub-1',
    tools: [],
    mcp_servers: [],
    slash_commands: [],
    agents: [],
    skills: [],
    plugins: [],
  }) + '\n',
);

// Ignore SIGHUP and SIGPIPE — without this Node's default would terminate
// us on the daemon's first kill signal, masking the N1 SIGKILL fallback.
process.on('SIGHUP', () => {
  /* intentionally swallowed */
});
process.on('SIGPIPE', () => {
  /* intentionally swallowed */
});

// Drain stdin but don't exit on close — the daemon's EOF-via-stdin-drop
// happy path is supposed to NOT kill us; only the group-SIGKILL should.
const rl = createInterface({ input: process.stdin });
rl.on('line', () => {
  /* ignore everything */
});
rl.on('close', () => {
  /* deliberately do NOT exit — wait for the SIGKILL fallback */
});

// 10 min — far longer than any test budget but bounded so a stuck CI
// run doesn't leak this process indefinitely if every signal path
// silently regresses.
setTimeout(() => process.exit(0), 10 * 60 * 1000);
