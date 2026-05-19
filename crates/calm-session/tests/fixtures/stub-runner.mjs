#!/usr/bin/env node
// Stub neige-chat-runner for daemon-level e2e tests. Implements the same
// stdio control protocol as the real runner but never touches the SDK; on
// each user_message it writes a Passthrough NeigeEvent echoing the content.
// Used by crates/calm-session/tests/chat_e2e.rs.

import { createInterface } from 'node:readline';
import process from 'node:process';

const argv = process.argv.slice(2);
let sessionId = '00000000-0000-0000-0000-000000000000';
let cwd = '';
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === '--session-id' && argv[i + 1]) sessionId = argv[i + 1];
  if (argv[i] === '--cwd' && argv[i + 1]) cwd = argv[i + 1];
}

function emit(ev) {
  process.stdout.write(JSON.stringify(ev) + '\n');
}

emit({
  type: 'session_init',
  session_id: sessionId,
  model: 'stub',
  permission_mode: 'default',
  cwd,
  version: 'stub-1',
  tools: [],
  mcp_servers: [],
  slash_commands: [],
  agents: [],
  skills: [],
  plugins: [],
});

const rl = createInterface({ input: process.stdin });
rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let frame;
  try {
    frame = JSON.parse(trimmed);
  } catch (e) {
    process.stderr.write(`stub: parse error: ${e}\n`);
    return;
  }
  switch (frame.kind) {
    case 'user_message':
      emit({
        type: 'passthrough',
        session_id: sessionId,
        kind: 'stub_echo',
        payload: { content: frame.content },
      });
      break;
    case 'stop':
      emit({
        type: 'passthrough',
        session_id: sessionId,
        kind: 'stub_stop',
        payload: {},
      });
      process.exit(0);
    case 'answer_question':
      emit({
        type: 'passthrough',
        session_id: sessionId,
        kind: 'stub_answer',
        payload: { question_id: frame.question_id, answers: frame.answers },
      });
      break;
    default:
      process.stderr.write(`stub: unknown kind: ${frame.kind}\n`);
  }
});
rl.on('close', () => process.exit(0));
