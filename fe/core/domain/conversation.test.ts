import { describe, expect, it } from 'vitest';

import type { HarnessItem } from '../api/generated/wire.js';
import {
  PLAN_LIST_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS, TASK_VERDICT_TOOL, WAVE_RENAME_TOOL,
  WAVE_TOOL_PREFIX,
} from '../keys/mcp-tools.js';

import type { ApiFailure } from '../api/types.js';
import {
  buildTranscript, CONVERSATION_NAME_MAX, conversationName, conversationNameFrom,
  CONVERSATION_STATE_SOURCE,
  coveConversationCardId, coveConversationFailure, coveConversationsOperation,
  createCoveConversationOperation, createWaveConversationOperation,
  harnessItemToActivity, harnessItemToTurn, isLiveConversation, mergeTranscript, readableCommand,
  reconcileUserEchoes, toCoveConversation, toWaveConversation, waveConversationCardId,
  waveConversationsOperation,
  type Conversation, type ConversationKind, type ConversationTurn,
} from './conversation.js';

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', title: null,
    kind: 'codex', state: 'idle', updatedAt: 0, turns: 0,
    ...overrides,
  };
}

describe('conversationName', () => {
  it('prefers the conversation\'s own name', () => {
    expect(conversationName(conversation({ title: 'Why the resolver drops a hop' })))
      .toBe('Why the resolver drops a hop');
  });

  it('falls back to the kind, never to the wave', () => {
    const nameless = conversation({ waveTitle: 'Ship the rewrite' });
    expect(conversationName(nameless)).toBe('Codex');
    expect(conversationName(nameless)).not.toBe('Ship the rewrite');
  });
});

describe('reconcileUserEchoes', () => {
  const turn = (id: string, text: string): ConversationTurn => ({ id, author: 'you', text, atMs: 1 });

  it('lets one server row consume only one of two identical echoes', () => {
    expect(reconcileUserEchoes(
      [turn('server-1', 'same')],
      [turn('echo-1', 'same'), turn('echo-2', 'same')],
    ).map((entry) => entry.id)).toEqual(['echo-2']);
  });

  it('does not reconcile against server rows outside the bounded lookback', () => {
    const rows = [turn('old', 'same'), ...Array.from({ length: 50 }, (_, index) => turn(`recent-${index}`, `text-${index}`))];
    expect(reconcileUserEchoes(rows, [turn('echo', 'same')])).toHaveLength(1);
  });
});

describe('conversationNameFrom', () => {
  it('takes the first line, not the first paragraph', () => {
    expect(conversationNameFrom('Fix the resolver\n\nHere is the stack trace:\n  at walk()'))
      .toBe('Fix the resolver');
  });

  it('truncates to one name-length, ellipsis included in the budget', () => {
    const name = conversationNameFrom('x'.repeat(200));
    expect(name).toHaveLength(CONVERSATION_NAME_MAX);
    expect(name?.endsWith('…')).toBe(true);
  });

  it('leaves a name that already fits exactly alone', () => {
    const exact = 'y'.repeat(CONVERSATION_NAME_MAX);
    expect(conversationNameFrom(exact)).toBe(exact);
  });

  it.each([['empty', ''], ['whitespace', '   \n  ']])('has no name for a %s message', (_label, text) => {
    expect(conversationNameFrom(text)).toBeNull();
  });
});

describe('cove conversations', () => {
  const row = {
    id: 'card-9', waveId: 'chat-wave', title: null, kind: 'shared-chat',
    state: 'idle' as const, updatedAt: 42,
  };

  it('names a nameless cove conversation Chat, never after its hidden wave', () => {
    expect(conversationName(toCoveConversation(row))).toBe('Chat');
  });

  it('leaves what the server does not send absent rather than inventing it', () => {
    const conversation = toCoveConversation(row);
    expect(conversation.waveTitle).toBeUndefined();
    expect(conversation.turns).toBeUndefined();
    expect(Object.hasOwn(conversation, 'waveTitle')).toBe(false);
    expect(Object.hasOwn(conversation, 'turns')).toBe(false);
  });

  it('keeps a null state null: no session read is not a session that died', () => {
    expect(toCoveConversation({ ...row, state: null }).state).toBeNull();
    expect(isLiveConversation(null)).toBe(false);
    expect(isLiveConversation('turn_pending')).toBe(true);
  });

  it('sends the idempotency key as a header, and the text as the whole body', () => {
    const operation = createCoveConversationOperation('cove 1', 'hello', 'key-1');
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/coves/cove%201/conversations');
    expect(operation.headers).toEqual({ 'Idempotency-Key': 'key-1' });
    expect(operation.body).toEqual({ text: 'hello' });
  });

  it('decodes a list row into the app\'s own shape', () => {
    const parsed = coveConversationsOperation('c1').responseSchema.parse([row]);
    expect(parsed).toEqual([{
      id: 'card-9', waveId: 'chat-wave', title: null, kind: 'shared-chat',
      state: 'idle', updatedAt: 42,
    }]);
  });

  const http = (status: number, code: string, message: string): ApiFailure =>
    ({ kind: status === 401 ? 'unauthorized' : 'http', status, code, message } as ApiFailure);

  /* Not one of these is "409 means it already worked". Three of the four 409s
     have no conversation behind them, and each one leaves the draft in a
     different place. */
  it.each([
    ['no claimed folder', http(409, 'conflict', 'cove c1 has no claimed folder'), 'blocked'],
    ['a spent key', http(409, 'idempotency_key_exhausted', 'key exhausted'), 'exhausted'],
    ['an edited body', http(409, 'conflict', 'operation idempotency key k already used with different payload'), 'stale-payload'],
    ['a card that exists', http(409, 'conflict', 'card already exists'), 'exists'],
    ['a missing cove', http(404, 'not_found', 'cove not found'), 'gone'],
    ['a rejected request', http(400, 'bad_request', 'text must not be blank'), 'blocked'],
    ['a server error', http(500, 'internal', 'boom'), 'retry'],
    /* A separate kind for its *sentence*, not for a different resolution: on
       this endpoint a 503 is raised while delivering the first message, i.e.
       after the card was minted, so it is every bit as ambiguous as a 500 and
       the panel resolves both the same way. */
    ['a stopped agent server', http(503, 'codex_app_server', 'not running'), 'unavailable'],
    ['an unavailable service', http(503, 'service_unavailable', 'try later'), 'unavailable'],
  ])('reads %s as %s', (_label, failure, expected) => {
    expect(coveConversationFailure(failure).kind).toBe(expected);
  });

  it('treats a lost connection as ambiguous, not as a refusal', () => {
    expect(coveConversationFailure({ kind: 'transport', message: 'Transport request failed' }))
      .toEqual({ kind: 'retry', message: 'Transport request failed' });
  });

  /*
   * A golden, and the value is not ours: it is copied from the server's own
   * golden assertion — `the_derived_card_id_depends_only_on_cove_and_idempotency_key`
   * in `crates/calm-server/src/routes/cove_conversations.rs`, which pins
   * `("cove-1", "key-a")` to this exact id.
   *
   * Asserting the two sides agree is the entire point. A self-consistent
   * derivation (`derive(a) === derive(a)`) would stay green while the client
   * looked for a card id the server never mints — and the visible symptom of
   * that is not an error but a *silence*: the draft would never recognise its
   * own row and would keep offering to send it again.
   */
  it('derives the same card id the server does', () => {
    expect(coveConversationCardId('cove-1', 'key-a')).toBe('conv-7b12bb251f95129865ab81128125cbf5');
    expect(coveConversationCardId('cove-1', 'key-b')).not.toBe(coveConversationCardId('cove-1', 'key-a'));
    expect(coveConversationCardId('cove-2', 'key-a')).not.toBe(coveConversationCardId('cove-1', 'key-a'));
  });
});

describe('wave conversations', () => {
  const row = {
    id: 'card-3', waveId: 'wave-1', title: null, kind: 'wave-assistant',
    state: 'starting' as const, updatedAt: 7,
  };

  it('decodes a list row into the app\'s own shape', () => {
    const operation = waveConversationsOperation('wave 1');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/waves/wave%201/conversations');
    expect(operation.responseSchema.parse([row])).toEqual([{
      id: 'card-3', waveId: 'wave-1', title: null, kind: 'wave-assistant',
      state: 'starting', updatedAt: 7,
    }]);
  });

  /*
   * A row is rejected, not coerced. `state` is the field that matters: it is
   * the one the list renders a live dot from, and the server's contract is that
   * it is either one of the seven session states or `null` because the LEFT
   * JOIN found no session. A schema that let an unknown string through would
   * hand the renderer a state nobody defined behaviour for.
   */
  it('rejects a row whose session state is not one the kernel can produce', () => {
    expect(waveConversationsOperation('w').responseSchema.safeParse([{ ...row, state: 'dormant' }]).success)
      .toBe(false);
    expect(waveConversationsOperation('w').responseSchema.safeParse([{ ...row, state: null }]).success)
      .toBe(true);
  });

  it('leaves the wave title absent rather than inventing one, and names the row Assistant', () => {
    const conversation = toWaveConversation(row);
    expect(conversationName(conversation)).toBe('Assistant');
    expect(Object.hasOwn(conversation, 'waveTitle')).toBe(false);
    expect(Object.hasOwn(conversation, 'turns')).toBe(false);
  });

  /*
   * The two lists are separate kinds, not one kind with two sources. The
   * server sends distinct markers and says the frontend branches on them; a
   * transform that collapsed them would route assistant rows through the cove
   * chat's presentation, which is the exact mistake #1189 §4.1 warns about.
   */
  it('does not collapse into the cove chat kind', () => {
    expect(toWaveConversation(row).kind).not.toBe(toCoveConversation({ ...row, kind: 'shared-chat' }).kind);
  });

  it('posts the first message to the wave, carrying the key as a header', () => {
    const operation = createWaveConversationOperation('wave 1', 'hello', 'key-a');
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/waves/wave%201/conversations');
    expect(operation.body).toEqual({ text: 'hello' });
    expect(operation.headers).toEqual({ 'Idempotency-Key': 'key-a' });
    expect(operation.responseSchema.parse(row).kind).toBe('wave-assistant');
  });

  /*
   * A golden, and the value is the server's: it is copied from
   * `the_derived_card_id_depends_only_on_wave_and_idempotency_key` in
   * `crates/calm-server/src/conversation_keys.rs`, whose doc comment names this
   * function as the mirror it must be written against.
   *
   * The last assertion is the one that would survive a plausible mistake. The
   * two derivations differ **only** in the namespace inside the hashed string —
   * the visible `conv-` prefix is deliberately identical — so a wave derivation
   * that reused the cove prefix produces a perfectly well-shaped id that names
   * another endpoint's card, and a draft that adopted it would open a cove chat
   * as if it were the words just typed.
   */
  it('derives the same card id the server does, from its own namespace', () => {
    expect(waveConversationCardId('wave-1', 'key-a')).toBe('conv-9778c6de9be6196b5b44fdd411e5c305');
    expect(waveConversationCardId('wave-1', 'key-b')).not.toBe(waveConversationCardId('wave-1', 'key-a'));
    expect(waveConversationCardId('wave-2', 'key-a')).not.toBe(waveConversationCardId('wave-1', 'key-a'));
    expect(waveConversationCardId('id-1', 'key-a')).not.toBe(coveConversationCardId('id-1', 'key-a'));
  });
});

/*
 * Every kind says who owns its `state`, and the table is total.
 *
 * The router branches on this to decide whether a row's state is the server's
 * reading or the route's own phase, and the branch it replaces was
 * `kind === 'shared-chat' ? … : …`: a kind added to the union fell into the
 * `else` and had the server's state silently swapped for an invented one. The
 * `Record` makes that a compile error; this test makes it one at runtime too,
 * for the same reason `register.contract.test.ts` re-checks `headless` — a type
 * assertion can forge the compile-time half.
 */
describe('CONVERSATION_STATE_SOURCE', () => {
  const KINDS: readonly ConversationKind[] = [
    'terminal', 'codex', 'claude', 'shared-spec', 'shared-chat', 'wave-assistant',
  ];

  it('decides every kind, and only those', () => {
    expect(Object.keys(CONVERSATION_STATE_SOURCE).sort()).toEqual([...KINDS].sort());
    for (const kind of KINDS) expect(CONVERSATION_STATE_SOURCE[kind]).toMatch(/^(server|route)$/);
  });

  /* The two server-listed kinds are exactly the two that arrive from a list
     endpoint. Written out rather than derived, so widening it is a decision
     somebody has to make here. */
  it('names the listed kinds as the server\'s to report', () => {
    expect(KINDS.filter((kind) => CONVERSATION_STATE_SOURCE[kind] === 'server'))
      .toEqual(['shared-chat', 'wave-assistant']);
  });
});

function item(overrides: Partial<HarnessItem> = {}): HarnessItem {
  return {
    id: 7, runtime_id: 'runtime', card_id: 'card', wave_id: 'wave', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'item', item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ completedAtMs: 99, item: { text: 'answer' } }), created_at_ms: 50,
    ...overrides,
  };
}

describe('harnessItemToTurn', () => {
  it('maps completed agent messages', () => {
    expect(harnessItemToTurn(item())).toEqual({ id: '7', author: 'agent', text: 'answer', atMs: 99 });
  });

  it('strips the injected wave diff and user marker', () => {
    const text = '## Wave state changes since your last turn\nchanged\n\n---\n\nUser says:\nhello';
    expect(harnessItemToTurn(item({
      item_type: 'userMessage', params: JSON.stringify({ item: { content: [{ type: 'text', text }] } }),
    }))).toMatchObject({ author: 'you', text: 'hello' });
  });

  it('drops incomplete and unsupported entries', () => {
    expect(harnessItemToTurn(item({ method: 'item/started' }))).toBeNull();
    expect(harnessItemToTurn(item({ item_type: 'commandExecution' }))).toBeNull();
    expect(harnessItemToTurn(item({ params: '{broken' }))).toBeNull();
  });

  /*
   * Both rows below are **verbatim** captures from a live stack (`GET
   * /api/cards/{id}/harness/items`), not hand-written shapes. The bug this
   * guards against was invisible to hand-written fixtures precisely because the
   * fixtures repeated the same invented spelling as the code under test.
   */
  describe('captured wire rows', () => {
    const captured = (id: number, itemType: string, params: string): HarnessItem =>
      item({ id, item_type: itemType, params, created_at_ms: 1786763298839 });

    it('reads a real agent message', () => {
      const row = captured(6, 'agentMessage', '{"completedAtMs":1786763298838,"item":{"id":"msg_0276","memoryCitation":null,"phase":"commentary","text":"我先确认这个 Wave 的当前状态。","type":"agentMessage"},"threadId":"01a0","turnId":"01a0"}');
      expect(harnessItemToTurn(row)).toEqual({
        id: '6', author: 'agent', text: '我先确认这个 Wave 的当前状态。', atMs: 1786763298838,
      });
    });

    it('reads a real user message and keeps only what the human typed', () => {
      const row = captured(28, 'userMessage', JSON.stringify({
        completedAtMs: 1786763341752,
        item: {
          clientId: null,
          content: [{
            text: '## Wave state changes since your last turn (HEAD 32f19e5d -> 552cbdc9)\n- report.md edited\n\n---\n\nUser says:\nhello',
            text_elements: [], type: 'text',
          }],
          id: '01a0', type: 'userMessage',
        },
      }));
      expect(harnessItemToTurn(row)).toEqual({
        id: '28', author: 'you', text: 'hello', atMs: 1786763341752,
      });
    });
  });
});

describe('harnessItemToActivity', () => {
  const row = (overrides: Partial<HarnessItem>): HarnessItem => ({
    id: 7, runtime_id: 'runtime', card_id: 'card', wave_id: 'wave', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'uuid', item_type: 'commandExecution', method: 'item/completed',
    params: '{}', created_at_ms: 50, ...overrides,
  });

  it('reads a captured shell run and drops the bash wrapper', () => {
    // Verbatim shape from a live stack, trimmed of its 3KB of output.
    const activity = harnessItemToActivity(row({
      params: JSON.stringify({
        completedAtMs: 1786763301566,
        item: {
          command: "/usr/bin/bash -lc 'neige state'", aggregatedOutput: '{"cards": []}',
          exitCode: 0, durationMs: 120, status: 'completed', type: 'commandExecution',
        },
      }),
    }));
    expect(activity).toMatchObject({
      verb: 'Ran', target: 'neige state', state: 'done', durationMs: 120,
    });
  });

  /*
   * ── The two fields the wire always had ──────────────────────────────────
   *
   * `durationMs` and `aggregatedOutput` were never missing from `item/completed`
   * — the capture above has both, and the kernel stores `params` unfiltered.
   * This function was where they died. These cases pin the two halves of the
   * rule that brought them back: the number survives on every completed row,
   * and the text is shown **only** where it is the answer to a question the
   * reader is actually asking.
   */
  const shellRun = (item: Record<string, unknown>): HarnessItem => row({
    params: JSON.stringify({ completedAtMs: 1786763301566, item: { type: 'commandExecution', ...item } }),
  });

  it('says why a shell run failed, in its last line of output', () => {
    expect(harnessItemToActivity(shellRun({
      command: "/usr/bin/bash -lc 'npm test'",
      aggregatedOutput: '> vitest run\n\nFAIL core/domain/conversation.test.ts\n\nTests  1 failed | 40 passed\n\n',
      exitCode: 1, durationMs: 8_400, status: 'completed',
    }))).toMatchObject({
      state: 'failed', detail: 'Tests  1 failed | 40 passed', durationMs: 8_400,
    });
  });

  /*
   * The assertion that pins "failure-only". The successful row below carries a
   * perfectly readable `aggregatedOutput`, and it is still dropped: in a real
   * session there are three successful actions for every sentence, and a tail
   * of stdout under each of them is the drawer turning into a log viewer.
   */
  it('drops the output of a run that succeeded, even though it is right there', () => {
    expect(harnessItemToActivity(shellRun({
      command: 'ls', aggregatedOutput: 'report.md\nnotes.md\n', exitCode: 0,
      durationMs: 30, status: 'completed',
    }))).toMatchObject({ state: 'done', detail: null, durationMs: 30 });
  });

  /* `aggregatedOutput` is the whole capture — kilobytes on a real build. The
     field is typed as one short line, so the clip is the domain's job. */
  it('clips a failure reason to one short line instead of a payload', () => {
    const detail = harnessItemToActivity(shellRun({
      command: 'build', aggregatedOutput: `ok\n${'x'.repeat(4_000)}`, exitCode: 2,
      status: 'completed',
    }))?.detail;
    expect(detail).not.toBeNull();
    expect(detail!.length).toBeLessThanOrEqual(64);
    expect(detail!.endsWith('…')).toBe(true);
  });

  /* MCP tools have no stdout at all; their reason is the `error` member, and
     both spellings of it are on our wire. */
  it.each([
    ['an object with a message', { message: 'wave is not attached' }],
    ['a bare string', 'wave is not attached'],
  ])('falls back to the mcp error when it is %s', (_label, error) => {
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({
        item: { tool: REPORT_WRITE_TOOLS[0], error, status: 'failed', durationMs: 45, type: 'mcpToolCall' },
      }),
    }))).toMatchObject({ state: 'failed', detail: 'wave is not attached', durationMs: 45 });
  });

  it('has no duration on a row that has not finished', () => {
    expect(harnessItemToActivity(row({
      method: 'item/started',
      params: JSON.stringify({ item: { command: 'ls', type: 'commandExecution' } }),
    }))).toMatchObject({ state: 'running', durationMs: null, detail: null });
  });

  it('says the report was written, because that is the answer', () => {
    const activity = harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({
        completedAtMs: 1786763335477,
        item: {
          server: 'calm', tool: REPORT_WRITE_TOOLS[3], arguments: { body: '# 概要' },
          error: null, status: 'completed', durationMs: 15, type: 'mcpToolCall',
        },
      }),
    }));
    expect(activity).toMatchObject({ verb: 'Wrote report', target: null, state: 'done' });
  });

  it('tells a read of the report apart from a write of it', () => {
    const read = harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({ item: { tool: REPORT_READ_TOOLS[0], status: 'completed' } }),
    }));
    expect(read).toMatchObject({ verb: 'Read report', state: 'done' });
  });

  it.each([
    [TASK_VERDICT_TOOL, 'Writing task verdict', 'Wrote task verdict'],
    [PLAN_LIST_TOOL, 'Reading plan', 'Read plan'],
  ])('renders the known %s tool in English', (tool, running, done) => {
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall', method: 'item/started',
      params: JSON.stringify({ item: { tool } }),
    }))?.verb).toBe(running);
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall', params: JSON.stringify({ item: { tool } }),
    }))?.verb).toBe(done);
  });

  // #1211 S3 — `calm.wave.rename` is the first `calm.wave.*` tool that WRITES.
  // It used to fall into the prefix bucket and read out as "Read the wave",
  // which is exactly backwards on the one line a user scans to find out who
  // named their wave.
  it('renders the wave rename as a write, not as a look at the wave', () => {
    expect(WAVE_RENAME_TOOL.startsWith(WAVE_TOOL_PREFIX)).toBe(true);
    const started = harnessItemToActivity(row({
      item_type: 'mcpToolCall', method: 'item/started',
      params: JSON.stringify({ item: { tool: WAVE_RENAME_TOOL } }),
    }));
    const done = harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({ item: { tool: WAVE_RENAME_TOOL, status: 'completed' } }),
    }));
    expect(started).toMatchObject({ verb: 'Naming the wave', target: null, state: 'running' });
    expect(done).toMatchObject({ verb: 'Named the wave', target: null, state: 'done' });
    for (const activity of [started, done]) {
      expect(activity?.verb).not.toMatch(/read/i);
    }
  });

  it('still reads the other `calm.wave.*` tools as looks', () => {
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({ item: { tool: `${WAVE_TOOL_PREFIX}state`, status: 'completed' } }),
    }))).toMatchObject({ verb: 'Read the wave', state: 'done' });
  });

  it('is running while only `item/started` has arrived', () => {
    expect(harnessItemToActivity(row({
      method: 'item/started',
      params: JSON.stringify({ item: { command: 'ls', type: 'commandExecution' } }),
    }))).toMatchObject({ verb: 'Running', state: 'running' });
  });

  it('does not claim that a started file change edited zero files', () => {
    expect(harnessItemToActivity(row({
      item_type: 'fileChange', method: 'item/started',
      params: JSON.stringify({ item: { type: 'fileChange' } }),
    }))).toMatchObject({ verb: 'Editing', target: null, state: 'running' });
  });

  it.each([
    ['subAgentActivity', 'Delegating', 'Delegated'],
    ['dynamicToolCall', 'Calling tool', 'Called tool'],
    ['hookPrompt', 'Prompting', 'Prompted'],
    ['imageView', 'Viewing image', 'Viewed image'],
    ['enteredReviewMode', 'Entering review mode', 'Entered review mode'],
    ['exitedReviewMode', 'Exiting review mode', 'Exited review mode'],
    ['contextCompaction', 'Compacting', 'Compacted'],
  ])('renders the known %s item type', (itemType, running, done) => {
    const started = harnessItemToActivity(row({
      item_type: itemType, method: 'item/started', params: JSON.stringify({ item: {} }),
    }));
    const completed = harnessItemToActivity(row({
      item_type: itemType, params: JSON.stringify({ item: {} }),
    }));
    expect(started?.verb).toBe(running);
    expect(completed?.verb).toBe(done);
  });

  it.each([
    ['a non-zero exit', { command: 'false', exitCode: 1, status: 'completed' }],
    ['an mcp error member', { tool: REPORT_WRITE_TOOLS[0], error: { message: 'nope' }, status: 'completed' }],
    ['a failed status', { command: 'x', exitCode: 0, status: 'failed' }],
  ])('reads failure from %s', (_label, item) => {
    const itemType = 'tool' in item || 'error' in item ? 'mcpToolCall' : 'commandExecution';
    expect(harnessItemToActivity(row({
      item_type: itemType, params: JSON.stringify({ item }),
    }))?.state).toBe('failed');
  });

  it('renders a neutral line for an item type this build has never seen', () => {
    expect(harnessItemToActivity(row({
      item_type: 'somethingNewInCodex', params: JSON.stringify({ item: {} }),
    }))).toMatchObject({ verb: 'Worked', target: 'somethingNewInCodex', state: 'done' });
  });

  it('renders web search as an outside-world read instead of the generic fallback', () => {
    expect(harnessItemToActivity(row({
      item_type: 'webSearch', params: JSON.stringify({ item: {} }),
    }))).toMatchObject({ verb: 'Searched the web', target: null, state: 'done' });
  });

  it('strips the wrapper only when the whole command is one quoted string', () => {
    expect(readableCommand("/usr/bin/bash -lc 'neige ls /'")).toBe('neige ls /');
    expect(readableCommand('bash -c "npm test"')).toBe('npm test');
    expect(readableCommand('git status')).toBe('git status');
  });
});

describe('buildTranscript', () => {
  const row = (id: number, itemType: string, method: string, item: unknown, uuid = `u${id}`): HarnessItem => ({
    id, runtime_id: 'r', card_id: 'c', wave_id: 'w', thread_id: 't', turn_id: 'turn',
    item_uuid: uuid, item_type: itemType, method,
    params: JSON.stringify({ completedAtMs: 1000 + id, item }), created_at_ms: 1000 + id,
  });

  it('pairs started with completed into one line, in the started position', () => {
    const entries = buildTranscript([
      row(1, 'commandExecution', 'item/started', { command: 'ls', type: 'commandExecution' }),
      row(2, 'agentMessage', 'item/completed', { text: 'done', type: 'agentMessage' }, 'u-msg'),
      row(3, 'commandExecution', 'item/completed', { command: 'ls', exitCode: 0 }, 'u1'),
    ]);
    expect(entries.map((entry) => (entry.author === 'activity' ? entry.verb : entry.text)))
      .toEqual(['Ran', 'done']);
  });

  it('keeps thinking while it is the last thing, and drops it once anything follows', () => {
    const thinking = [
      row(1, 'reasoning', 'item/completed', { summary: [], type: 'reasoning' }, 'u1'),
      row(2, 'reasoning', 'item/completed', { summary: [], type: 'reasoning' }, 'u2'),
    ];
    expect(buildTranscript(thinking).map((entry) => entry.author === 'activity' && entry.verb))
      .toEqual(['Thought']);
    const answered = [...thinking, row(3, 'agentMessage', 'item/completed', { text: 'hi' }, 'u3')];
    expect(buildTranscript(answered).map((entry) => (entry.author === 'activity' ? entry.verb : entry.text)))
      .toEqual(['hi']);
  });

  it('orders by the wire id, not by arrival', () => {
    const entries = buildTranscript([
      row(9, 'agentMessage', 'item/completed', { text: 'second' }, 'u9'),
      row(4, 'userMessage', 'item/completed', { content: [{ text: 'first' }] }, 'u4'),
    ]);
    expect(entries.map((entry) => (entry.author === 'activity' ? entry.verb : entry.text)))
      .toEqual(['first', 'second']);
  });

  it('renders snake_case messages as turns, not generic activities', () => {
    const entries = buildTranscript([
      row(1, 'user_message', 'item/completed', { content: [{ text: 'question' }] }),
      row(2, 'agent_message', 'item/completed', { text: 'answer' }),
    ]);
    expect(entries).toMatchObject([
      { author: 'you', text: 'question' },
      { author: 'agent', text: 'answer' },
    ]);
    expect(entries.every((entry) => entry.author !== 'activity')).toBe(true);
  });

  it('does not render a started agent message as an activity', () => {
    expect(buildTranscript([
      row(1, 'agentMessage', 'item/started', { text: 'still arriving' }),
    ])).toEqual([]);
  });

  it('does not render empty completed messages as activities', () => {
    expect(buildTranscript([
      row(1, 'agentMessage', 'item/completed', { text: '' }),
      row(2, 'userMessage', 'item/completed', {
        content: [{ text: '## Wave state changes since your last turn\nchanged\n\n---\n\nUser says:\n' }],
      }),
    ])).toEqual([]);
  });
});

describe('mergeTranscript', () => {
  const thought = {
    id: 'thought', author: 'activity' as const, verb: 'Thought', target: null,
    state: 'done' as const, durationMs: null, detail: null, atMs: 1,
  };
  const echo: ConversationTurn = { id: 'echo', author: 'you', text: 'next', atMs: 2 };

  it('drops a completed tail thought when an echo follows it', () => {
    expect(mergeTranscript([thought], [echo])).toEqual([echo]);
  });

  it('keeps a completed tail thought until an echo exists', () => {
    expect(mergeTranscript([thought], [])).toEqual([thought]);
  });
});
