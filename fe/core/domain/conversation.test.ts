import { describe, expect, it } from 'vitest';

import type { HarnessItem } from '../api/generated/wire.js';
import {
  PLAN_LIST_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS, TASK_VERDICT_TOOL, TRACK_RENAME_TOOL,
  TRACK_TOOL_PREFIX,
} from '../keys/mcp-tools.js';

import {
  buildTranscript, CONVERSATION_NAME_MAX, conversationName, conversationNameFrom,
  CONVERSATION_STATE_SOURCE, conversationCreateFailure,
  createTrackConversationOperation,
  harnessItemToActivity, harnessItemToTurns as transcriptRowToMessages, isLiveConversation,
  mergeTranscript, readableCommand,
  reconcileUserEchoes, toTrackConversation, trackConversationCardId,
  trackConversationsOperation,
  type Conversation, type ConversationKind, type ConversationTurn,
} from './conversation.js';

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null,
    kind: 'codex', state: 'idle', updatedAt: 0, turns: 0,
    ...overrides,
  };
}

describe('conversationName', () => {
  it('prefers the conversation\'s own name', () => {
    expect(conversationName(conversation({ title: 'Why the resolver drops a hop' })))
      .toBe('Why the resolver drops a hop');
  });

  it('falls back to the kind, never to the track', () => {
    const nameless = conversation({ trackTitle: 'Ship the rewrite' });
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

describe('track conversations', () => {
  const row = {
    id: 'card-3', trackId: 'track-1', title: null, kind: 'track-assistant',
    state: 'starting' as const, updatedAt: 7,
  };

  it('decodes a list row into the app\'s own shape', () => {
    const operation = trackConversationsOperation('track 1');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/tracks/track%201/conversations');
    expect(operation.responseSchema.parse([row])).toEqual([{
      id: 'card-3', trackId: 'track-1', title: null, kind: 'track-assistant',
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
    expect(trackConversationsOperation('w').responseSchema.safeParse([{ ...row, state: 'dormant' }]).success)
      .toBe(false);
    expect(trackConversationsOperation('w').responseSchema.safeParse([{ ...row, state: null }]).success)
      .toBe(true);
    expect(isLiveConversation(null)).toBe(false);
    expect(isLiveConversation('turn_pending')).toBe(true);
  });

  it.each([
    [{ kind: 'http', status: 409, code: 'idempotency_key_exhausted', message: 'key exhausted' }, 'exhausted'],
    [{ kind: 'http', status: 409, code: 'conflict', message: 'already used with different payload' }, 'stale-payload'],
    [{ kind: 'http', status: 409, code: 'conflict', message: 'card already exists' }, 'exists'],
    [{ kind: 'http', status: 404, code: 'not_found', message: 'track not found' }, 'gone'],
    [{ kind: 'http', status: 400, code: 'bad_request', message: 'text must not be blank' }, 'blocked'],
    [{ kind: 'http', status: 503, code: 'service_unavailable', message: 'try later' }, 'unavailable'],
    [{ kind: 'transport', message: 'request failed' }, 'retry'],
  ] as const)('classifies conversation create failure %o as %s', (failure, expected) => {
    expect(conversationCreateFailure(failure).kind).toBe(expected);
  });

  it('leaves the track title absent rather than inventing one, and names the row Assistant', () => {
    const conversation = toTrackConversation(row);
    expect(conversationName(conversation)).toBe('Assistant');
    expect(Object.hasOwn(conversation, 'trackTitle')).toBe(false);
    expect(Object.hasOwn(conversation, 'turns')).toBe(false);
  });

  it('posts the first message to the track, carrying the key as a header', () => {
    const operation = createTrackConversationOperation('track 1', 'hello', 'key-a');
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/track%201/conversations');
    expect(operation.body).toEqual({ text: 'hello' });
    expect(operation.headers).toEqual({ 'Idempotency-Key': 'key-a' });
    expect(operation.responseSchema.parse(row).kind).toBe('track-assistant');
  });

  /*
   * A golden, and the value is the server's: it is copied from
   * `the_derived_card_id_depends_only_on_track_and_idempotency_key` in
   * `crates/calm-server/src/conversation_keys.rs`, whose doc comment names this
   * function as the mirror it must be written against.
   *
   */
  it('derives the same card id the server does, from its own namespace', () => {
    expect(trackConversationCardId('track-1', 'key-a')).toBe('conv-55cef7267426fe78493bdd46ca6b1220');
    expect(trackConversationCardId('track-1', 'key-b')).not.toBe(trackConversationCardId('track-1', 'key-a'));
    expect(trackConversationCardId('track-2', 'key-a')).not.toBe(trackConversationCardId('track-1', 'key-a'));
  });
});

/*
 * Every kind says who owns its `state`, and the table is total.
 *
 * The router branches on this to decide whether a row's state is the server's
 * reading or the route's own phase, and the branch it replaces was
 * `kind === 'track-assistant' ? … : …`: a kind added to the union fell into the
 * `else` and had the server's state silently swapped for an invented one. The
 * `Record` makes that a compile error; this test makes it one at runtime too,
 * for the same reason `register.contract.test.ts` re-checks `headless` — a type
 * assertion can forge the compile-time half.
 */
describe('CONVERSATION_STATE_SOURCE', () => {
  const KINDS: readonly ConversationKind[] = [
    'terminal', 'codex', 'claude', 'shared-spec', 'track-assistant',
  ];

  it('decides every kind, and only those', () => {
    expect(Object.keys(CONVERSATION_STATE_SOURCE).sort()).toEqual([...KINDS].sort());
    for (const kind of KINDS) expect(CONVERSATION_STATE_SOURCE[kind]).toMatch(/^(server|route)$/);
  });

  /* The server-listed kind is exactly the one that arrives from a list
     endpoint. Written out rather than derived, so widening it is a decision
     somebody has to make here. */
  it('names the listed kinds as the server\'s to report', () => {
    expect(KINDS.filter((kind) => CONVERSATION_STATE_SOURCE[kind] === 'server'))
      .toEqual(['track-assistant']);
  });
});

function item(overrides: Partial<HarnessItem> = {}): HarnessItem {
  return {
    id: 7, runtime_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'item', item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ completedAtMs: 99, item: { text: 'answer' } }), created_at_ms: 50,
    ...overrides,
  };
}

describe('transcriptRowToMessages', () => {
  it('maps completed agent messages', () => {
    expect(transcriptRowToMessages(item())).toEqual([
      { id: '7', author: 'agent', text: 'answer', atMs: 99 },
    ]);
  });

  it('strips the injected track diff and user marker', () => {
    const text = '## Track state changes since your last turn\nchanged\n\n---\n\nUser says:\nhello';
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage', params: JSON.stringify({ item: { content: [{ type: 'text', text }] } }),
    }))).toMatchObject([{ author: 'you', text: 'hello' }]);
  });

  it.each([
    ['system_worker_turn_finished', 'Worker turn finished'],
    ['system_report_edited', 'Report edited'],
    ['system_task_completed', 'Task completed'],
    ['system_task_failed', 'Task failed'],
    ['system', 'System update'],
  ] as const)('uses structured %s metadata for the system label', (inputPresentation, label) => {
    const text = 'wording may change without changing who authored this';
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage', input_segments: [{ presentation: inputPresentation, text }],
      params: '{broken upstream frame',
    }))).toEqual([{ id: '7', author: 'system', label, text, atMs: 50 }]);
  });

  it('uses ordered segments instead of the diff-prefixed flattened echo', () => {
    const flattened = '## Track state changes since your last turn\nchanged\n\n---\n\nnew wording';
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage',
      input_segments: [{ presentation: 'system_report_edited', text: 'new wording' }],
      params: JSON.stringify({
        completedAtMs: 99, item: { content: [{ type: 'text', text: flattened }] },
      }),
    }))).toEqual([{
      id: '7', author: 'system', label: 'Report edited', text: 'new wording', atMs: 99,
    }]);
  });

  it('never infers system authorship from English text', () => {
    const text = 'A dispatched task completed, according to the user';
    for (const inputSegments of [[{ presentation: 'user' as const, text }], undefined]) {
      expect(transcriptRowToMessages(item({
        item_type: 'userMessage', input_segments: inputSegments,
        params: JSON.stringify({ item: { content: [{ type: 'text', text }] } }),
      }))).toMatchObject([{ author: 'you', text }]);
    }
  });

  it.each([
    [
      [
        { presentation: 'system_report_edited' as const, text: 'report changed' },
        { presentation: 'user' as const, text: 'User says:\nhello' },
      ],
      ['system', 'you'],
      ['report changed', 'hello'],
    ],
    [
      [
        { presentation: 'user' as const, text: 'User says:\nhello' },
        { presentation: 'system_task_completed' as const, text: 'task completed' },
      ],
      ['you', 'system'],
      ['hello', 'task completed'],
    ],
  ])('keeps mixed segment order without attributing system text to the user', (
    inputSegments, authors, texts,
  ) => {
    const turns = transcriptRowToMessages(item({
      item_type: 'userMessage', input_segments: inputSegments,
      params: JSON.stringify({ item: { content: [{ type: 'text', text: 'flattened' }] } }),
    }));
    expect(turns.map((turn) => turn.author)).toEqual(authors);
    expect(turns.map((turn) => turn.text)).toEqual(texts);
    expect(reconcileUserEchoes(turns, [
      { id: 'echo', author: 'you', text: 'hello', atMs: 100 },
    ])).toEqual([]);
  });

  it('drops incomplete and unsupported entries', () => {
    expect(transcriptRowToMessages(item({ method: 'item/started' }))).toEqual([]);
    expect(transcriptRowToMessages(item({ item_type: 'commandExecution' }))).toEqual([]);
    expect(transcriptRowToMessages(item({ params: '{broken' }))).toEqual([]);
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
      const row = captured(6, 'agentMessage', '{"completedAtMs":1786763298838,"item":{"id":"msg_0276","memoryCitation":null,"phase":"commentary","text":"我先确认这个 Track 的当前状态。","type":"agentMessage"},"threadId":"01a0","turnId":"01a0"}');
      expect(transcriptRowToMessages(row)).toEqual([{
        id: '6', author: 'agent', text: '我先确认这个 Track 的当前状态。', atMs: 1786763298838,
      }]);
    });

    it('reads a real user message and keeps only what the human typed', () => {
      const row = captured(28, 'userMessage', JSON.stringify({
        completedAtMs: 1786763341752,
        item: {
          clientId: null,
          content: [{
            text: '## Track state changes since your last turn (HEAD 32f19e5d -> 552cbdc9)\n- report.md edited\n\n---\n\nUser says:\nhello',
            text_elements: [], type: 'text',
          }],
          id: '01a0', type: 'userMessage',
        },
      }));
      expect(transcriptRowToMessages(row)).toEqual([{
        id: '28', author: 'you', text: 'hello', atMs: 1786763341752,
      }]);
    });
  });
});

describe('harnessItemToActivity', () => {
  const row = (overrides: Partial<HarnessItem>): HarnessItem => ({
    id: 7, runtime_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
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

  /*
   * ── A stated reason outranks a guessed one ──────────────────────────────
   *
   * A killed or timed-out command carries both: `error` says why it stopped and
   * `aggregatedOutput` is a partial capture whose last line is whatever
   * progress happened to be printed before the axe fell. Reading the tail here
   * prints `Compiling serde v1.0.219` under a red `Failed` and never says the
   * word "timed out" — strictly worse than printing nothing at all.
   */
  it('says the machine’s own reason, not the tail it was cut off in', () => {
    expect(harnessItemToActivity(shellRun({
      command: 'cargo build',
      aggregatedOutput: '   Compiling serde v1.0.219\n',
      error: 'command timed out after 600s',
      exitCode: 124,
      status: 'completed',
    }))).toMatchObject({ state: 'failed', detail: 'command timed out after 600s' });
  });

  /* MCP tools have no stdout at all; their reason is the `error` member, and
     both spellings of it are on our wire. */
  it.each([
    ['an object with a message', { message: 'track is not attached' }],
    ['a bare string', 'track is not attached'],
  ])('reads the mcp error when it is %s', (_label, error) => {
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({
        item: { tool: REPORT_WRITE_TOOLS[0], error, status: 'failed', durationMs: 45, type: 'mcpToolCall' },
      }),
    }))).toMatchObject({ state: 'failed', detail: 'track is not attached', durationMs: 45 });
  });

  /*
   * ── The `Caused by:` chain, verbatim from the production database ────────
   *
   * These two messages are not constructed for the test: they are the `error`
   * member of *both* of the only two failed `mcpToolCall` rows the production
   * database has (`harness_items` 34080 and 34526), and neither of those rows
   * carries an `aggregatedOutput`, so `error` is the only source there is.
   * Their shape is the anyhow chain's: a generic wrapper naming the tool, then
   * `Caused by:`, then the root cause — which is the whole of what the reader
   * opened the line to find out, and which reading the message from the front
   * throws away. Same rule as the shell tail above, for the same reason: a
   * machine writes the thing it is finally reporting last.
   */
  const mcpFailure = (error: unknown): HarnessItem => row({
    item_type: 'mcpToolCall',
    params: JSON.stringify({
      item: { tool: REPORT_WRITE_TOOLS[0], error, status: 'failed', type: 'mcpToolCall' },
    }),
  });

  it('reads the root cause out of a `Caused by:` chain, not its wrapper', () => {
    expect(harnessItemToActivity(mcpFailure({
      message: 'tool call error: tool call failed for `calm/calm.report.edit`\n'
        + '\nCaused by:\n    Mcp error: -32602: message must be non-empty\n',
    }))).toMatchObject({ state: 'failed', detail: 'Mcp error: -32602: message must be non-empty' });
  });

  it('reads the root cause of the other failed row on the wire', () => {
    expect(harnessItemToActivity(mcpFailure({
      message: 'tool call error: tool call failed for `calm/calm.plan.upsert`\n'
        + '\nCaused by:\n    Mcp error: -32602: `tasks` must be a non-empty array\n',
    }))).toMatchObject({
      state: 'failed', detail: 'Mcp error: -32602: `tasks` must be a non-empty array',
    });
  });

  /* The one-line case the rule must leave exactly where it was: with nothing
     after it, the last non-empty line *is* the first one. */
  it('still says a single-line error whole', () => {
    expect(harnessItemToActivity(mcpFailure('track is not attached')))
      .toMatchObject({ state: 'failed', detail: 'track is not attached' });
  });

  /* The payload **carries** a `durationMs` here, and that is the whole point: a
     started row is the item as codex knew it at the start, nothing stops a
     number riding along on it, and a row still saying `Running` must not print
     an interval that has not ended. Fed a payload without the key, this case
     passes with or without the gate that enforces that — and what codex sends
     today is neither: a JSON `null`, which the `typeof` test rejects on its
     own. A number is the input that can tell the rule from the type check. */
  it('has no duration on a row that has not finished', () => {
    expect(harnessItemToActivity(row({
      method: 'item/started',
      params: JSON.stringify({
        item: { command: 'ls', durationMs: 5_000, type: 'commandExecution' },
      }),
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

  // #1211 S3 — `calm.track.rename` is the first `calm.track.*` tool that WRITES.
  // It used to fall into the prefix bucket and read out as "Read the track",
  // which is exactly backwards on the one line a user scans to find out who
  // named their track.
  it('renders the track rename as a write, not as a look at the track', () => {
    expect(TRACK_RENAME_TOOL.startsWith(TRACK_TOOL_PREFIX)).toBe(true);
    const started = harnessItemToActivity(row({
      item_type: 'mcpToolCall', method: 'item/started',
      params: JSON.stringify({ item: { tool: TRACK_RENAME_TOOL } }),
    }));
    const done = harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({ item: { tool: TRACK_RENAME_TOOL, status: 'completed' } }),
    }));
    expect(started).toMatchObject({ verb: 'Naming the track', target: null, state: 'running' });
    expect(done).toMatchObject({ verb: 'Named the track', target: null, state: 'done' });
    for (const activity of [started, done]) {
      expect(activity?.verb).not.toMatch(/read/i);
    }
  });

  it('still reads the other `calm.track.*` tools as looks', () => {
    expect(harnessItemToActivity(row({
      item_type: 'mcpToolCall',
      params: JSON.stringify({ item: { tool: `${TRACK_TOOL_PREFIX}state`, status: 'completed' } }),
    }))).toMatchObject({ verb: 'Read the track', state: 'done' });
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
    id, runtime_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn',
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

  it('expands one mixed user-message row into ordered transcript entries', () => {
    const mixed = {
      ...row(4, 'userMessage', 'item/completed', { content: [{ text: 'flattened' }] }, 'u4'),
      input_segments: [
        { presentation: 'system_report_edited' as const, text: 'report changed' },
        { presentation: 'user' as const, text: 'User says:\nhello' },
        { presentation: 'system_task_completed' as const, text: 'task completed' },
      ],
    };
    expect(buildTranscript([mixed])).toMatchObject([
      { id: '4:0', author: 'system', label: 'Report edited', text: 'report changed' },
      { id: '4:1', author: 'you', text: 'hello' },
      { id: '4:2', author: 'system', label: 'Task completed', text: 'task completed' },
    ]);
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

  /* The contract, not the mechanism: the transcript renders `item/started` and
     `item/completed` and nothing else. `turn/plan/updated` (#1255) is the row
     that made this worth stating — the kernel now writes codex's per-turn TODO
     checklist into the same table the frontend polls — but the assertion is
     deliberately written over *arbitrary* unknown methods, because a rule that
     only names today's method is a rule that a future method walks past.

     Note for anyone mutation-testing this — and this replaces an earlier note
     here that claimed the opposite: NO mutation of either filter turns this
     red, in either direction. `isTranscriptMethod` and the two converters
     (`transcriptRowToMessages`, `harnessItemToActivity`) are independent method
     filters that currently agree, and `buildTranscript` runs them in series.
     Delete the `isTranscriptMethod` gate and the converters still reject these
     rows; widen a converter to accept `item/updated` and the gate rejects the
     row before that converter is ever called. This test pins the *intent* of
     the allowlist — an unknown method renders nothing — but it cannot tell
     which filter did the work, and no test can while both filters stand. The
     converters must keep their own checks (they are exported and called
     directly, e.g. the message converter from `web/src/app/router/public.tsx`),
     so making one of them load-bearing here would mean weakening the other. */
  it('renders nothing for a method the transcript does not understand', () => {
    const unknownRow = (method: string, overrides: Partial<HarnessItem> = {}): HarnessItem => ({
      id: 2, runtime_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn',
      item_uuid: null, item_type: null, method,
      params: JSON.stringify({
        threadId: 't', turnId: 'turn-plan-1', explanation: null,
        plan: [{ step: 'audit', status: 'inProgress' }, { step: 'ship', status: 'pending' }],
      }),
      created_at_ms: 1002, ...overrides,
    });

    for (const method of ['turn/plan/updated', 'thread/realtime/sdp', 'item/updated']) {
      // The row as the kernel writes a plan: null item_uuid, null item_type.
      expect(buildTranscript([unknownRow(method)])).toEqual([]);

      // And with everything an `item/*` row would need to render — a known
      // `item_type` and a well-formed `{ completedAtMs, item }` envelope — so
      // that the *method* is provably the only reason nothing comes out.
      expect(buildTranscript([unknownRow(method, {
        item_type: 'commandExecution',
        params: JSON.stringify({ completedAtMs: 1002, item: { command: 'ls' } }),
      })])).toEqual([]);
    }

    // It also does not disturb the lines around it.
    expect(buildTranscript([
      row(1, 'userMessage', 'item/completed', { content: [{ text: 'go' }] }),
      unknownRow('turn/plan/updated'),
      row(3, 'agentMessage', 'item/completed', { text: 'done' }, 'u3'),
    ]).map((entry) => (entry.author === 'activity' ? entry.verb : entry.text)))
      .toEqual(['go', 'done']);
  });

  it('does not render empty completed messages as activities', () => {
    expect(buildTranscript([
      row(1, 'agentMessage', 'item/completed', { text: '' }),
      row(2, 'userMessage', 'item/completed', {
        content: [{ text: '## Track state changes since your last turn\nchanged\n\n---\n\nUser says:\n' }],
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
