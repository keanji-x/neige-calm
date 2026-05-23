#!/usr/bin/env node
// Silent stub runner for `chat_e2e::hello_chat_falls_through_on_silent_runner`.
// Models a broken / hung chat runner: never emits a frame on stdout, just
// drains stdin into the void until it gets EOF or `stop`. The daemon's
// chat-mode attach handshake must still complete within the bounded
// fallback timeout (`CHAT_FIRST_FRAME_TIMEOUT` in daemon.rs) with an
// empty `HelloChat.replay`, instead of hanging forever (see #243).

import { createInterface } from 'node:readline';
import process from 'node:process';

// Read stdin so the runner stays alive — but never write anything to
// stdout. `stop` makes the test able to tear us down cleanly without
// waiting on the daemon's own kill path.
const rl = createInterface({ input: process.stdin });
rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  try {
    const frame = JSON.parse(trimmed);
    if (frame.kind === 'stop') process.exit(0);
  } catch {
    // ignore parse errors
  }
});
rl.on('close', () => process.exit(0));
