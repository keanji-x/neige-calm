import { describe, expect, it } from 'vitest';

import type { HarnessItem, HarnessPhaseTag } from '../api/generated/wire.js';
import {
  PLAN_LIST_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS, TASK_VERDICT_TOOL, TRACK_RENAME_TOOL,
  TRACK_TOOL_PREFIX,
} from '../keys/mcp-tools.js';

import {
  buildTranscript, CONVERSATION_NAME_MAX, conversationName, conversationNameFrom,
  CONVERSATION_STATE_SOURCE, conversationCreateFailure,
  createSerialWriter, createTrackConversationOperation,
  harnessItemToActivity, harnessItemToTurnOutcome, harnessItemToTurns as transcriptRowToMessages,
  isLiveConversation,
  isOptimisticConversationTurn, isQueuedConversationTurn, kernelQueuesInput,
  mergeTranscript, plannerQueueWriteFailure, readableCommand,
  reconcileOptimisticConversationTurns, reconcileUserEchoes, serverItemHighWater,
  toTrackConversation, trackConversationCardId,
  trackConversationsOperation,
  type Conversation, type ConversationKind, type ConversationTurn, type OptimisticConversationTurn,
  type TranscriptEntry,
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

  /*
   * #1505 S6. An image with no words is the commonest thing this feature is
   * for, and its echo has an empty text. Text matching refuses two empty
   * strings — rightly — so without a second criterion that echo is never
   * reconciled, and an unreconciled echo blocks the composer for good.
   */
  const image = (id: string) => ({
    id, contentType: 'image/png', size: 3, url: `/api/cards/c/planner/attachments/${id}`,
  });
  const withImages = (id: string, text: string, ids: string[]): ConversationTurn => ({
    id, author: 'you', text, atMs: 1, attachments: ids.map(image),
  });

  it('reconciles a wordless echo against the row carrying the same image', () => {
    expect(reconcileUserEchoes(
      [withImages('server-1', '', ['a.png'])],
      [withImages('echo-1', '', ['a.png'])],
    )).toEqual([]);
  });

  it('does not reconcile a wordless echo against a row carrying a different image', () => {
    expect(reconcileUserEchoes(
      [withImages('server-1', '', ['b.png'])],
      [withImages('echo-1', '', ['a.png'])],
    ).map((entry) => entry.id)).toEqual(['echo-1']);
  });

  it('still refuses two wordless turns that carry nothing', () => {
    expect(reconcileUserEchoes(
      [turn('server-1', '')],
      [turn('echo-1', '')],
    ).map((entry) => entry.id)).toEqual(['echo-1']);
  });
});

describe('optimistic conversation provenance', () => {
  const serverTurn = (id: string, text = 'same'): ConversationTurn => ({ id, author: 'you', text, atMs: 1 });
  /* No default on `queued`: a helper that back-fills a newly required field is
     how a new guard gets hidden from the tests that should be exercising it. */
  const echo = (
    id: string, before: number, queued: boolean,
  ): OptimisticConversationTurn => ({
    id, author: 'you', text: 'same', atMs: 2, serverHighWaterBefore: before, queued,
    /* `entryId` IS defaulted, unlike `queued`, and the difference is which
       side a wrong value falls on. `queued` back-filled reads as "the kernel
       issued this", which is the dead-composer bug; `entryId: null` reads as
       "no queue entry claims this echo", which is what every case in this file
       is about — none of them has a pending queue at all. A test that needs a
       claimed echo spreads its own id over the result, and is then visibly
       doing so. */
    entryId: null,
  });

  it('takes the highest persisted item id as the pre-send boundary', () => {
    expect(serverItemHighWater([{ id: 4 }, { id: 9 }, { id: 2 }])).toBe(9);
    expect(serverItemHighWater([])).toBe(0);
  });

  it('does not let an identical older row confirm a newer echo', () => {
    expect(reconcileOptimisticConversationTurns([serverTurn('4')], [echo('echo-1', 4, false)]))
      .toHaveLength(1);
    expect(reconcileOptimisticConversationTurns([serverTurn('5')], [echo('echo-1', 4, false)]))
      .toHaveLength(0);
  });

  it('lets one new server row confirm only one of two identical echoes', () => {
    expect(reconcileOptimisticConversationTurns(
      [serverTurn('5')], [echo('echo-1', 4, false), echo('echo-2', 4, false)],
    ).map((turn) => turn.id)).toEqual(['echo-2']);
  });

  it('recognises only finite user turns carrying provenance', () => {
    const invalid = { ...echo('echo-2', 4, false), serverHighWaterBefore: Number.NaN };
    expect(isOptimisticConversationTurn(echo('echo-1', 4, false))).toBe(true);
    expect(isOptimisticConversationTurn(serverTurn('5'))).toBe(false);
    expect(isOptimisticConversationTurn(invalid)).toBe(false);
  });

  /*
   * The queue flag is a property of the *send*, so it has to be readable off a
   * merged transcript entry — the renderer holds `TranscriptEntry`, never the
   * echo type — and it has to be false for everything that is not an echo at
   * all. A server row has no `queued` field, and "no field" must not read as
   * "queued": that is the direction that would put the caption under a message
   * the agent already answered.
   */
  it('reads the queue flag only off an optimistic turn that carries it', () => {
    expect(isQueuedConversationTurn(echo('echo-1', 4, true))).toBe(true);
    expect(isQueuedConversationTurn(echo('echo-2', 4, false))).toBe(false);
    expect(isQueuedConversationTurn(serverTurn('5'))).toBe(false);
  });
});

/*
 * The kernel's whitelist, transcribed and asserted both ways.
 *
 * `HarnessState::can_issue_turn()` is `matches!(self, Self::Idle |
 * Self::TurnCompleted { .. })`, so these two issue at once and the other six
 * queue. Written as a table over the *whole* phase enum rather than as a couple
 * of spot checks: the failure this predicate exists to prevent is a phase that
 * nobody thought about landing on the wrong side, which only exhaustiveness can
 * catch. `resumed` is the one that reads wrong at a glance — the session
 * projection maps it to `Idle`, but `can_issue_turn` does not accept it.
 */
describe('kernelQueuesInput', () => {
  /*
   * ── The table is bound to the enum by the compiler, not by care ──────────
   *
   * `Record<HarnessPhaseTag, boolean>` is exhaustive in both directions: the
   * annotation rejects a missing key, and the `satisfies` rejects an extra one.
   * So a ninth phase cannot land quietly on either side of this table —
   * `tsc -b` is a gate, and it fails until someone decides which column the new
   * phase belongs in.
   *
   * **The `satisfies` is load-bearing and was not there at first.** Excess
   * properties are only checked against a *fresh* literal, and `Object.freeze`
   * is a function call, so with the annotation alone an unknown key passed
   * silently — measured, by adding one. The first version of this note claimed
   * both directions anyway. Both directions are now checked by mutation, one
   * per direction.
   *
   * `HarnessPhaseTag` is the right authority for that and the only one
   * available. `core/api/generated/wire.ts` is ts-rs output from the Rust enum,
   * and CI re-runs the generator and `git diff --exit-code`s the result, so the
   * type cannot drift from `HarnessState` without a second gate saying so. The
   * two hand-written zod copies of this list (`core/api/schemas.ts`,
   * and `harnessPhaseSchema` in the module under test) were *not* used: checking
   * a hand-written table against a hand-written list would look like a machine
   * check and be nothing of the kind.
   *
   * What it still cannot do, stated because the type gives no help here: it
   * pins the shape of the table, not the truth of a cell. Both columns are
   * `HarnessState::can_issue_turn()` read by a person.
   */
  const QUEUES: Readonly<Record<HarnessPhaseTag, boolean>> = Object.freeze({
    idle: false,
    turn_completed: false,
    pending_thread_start: true,
    issuing_turn: true,
    issuing_interrupt: true,
    turn_running: true,
    resumed: true,
    wedged: true,
  } satisfies Record<HarnessPhaseTag, boolean>);

  it.each(Object.entries(QUEUES))('%s queues: %s', (phase, queues) => {
    expect(kernelQueuesInput(phase as HarnessPhaseTag)).toBe(queues);
  });

  /* Unknown, and read as queueing on purpose — the note on the function has the
     asymmetry that decides it. Pinned so the reading is a decision rather than
     whatever the expression happens to do. */
  it('reads an unknown phase as queueing', () => {
    expect(kernelQueuesInput(null)).toBe(true);
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
    id: 7, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'item', item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ completedAtMs: 99, item: { text: 'answer' } }), created_at_ms: 50,
    ...overrides,
  };
}

describe('transcriptRowToMessages', () => {
  /*
   * #1505 S6 — the two halves of one rule, and the second half is why the
   * first cannot simply be "keep everything".
   */
  it('keeps a segment that is only an image, and still drops one that is nothing', () => {
    const attachment = {
      id: 'a.png', contentType: 'image/png', size: 3,
      url: '/api/cards/card/planner/attachments/a.png',
    };
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage',
      input_segments: [{ presentation: 'user', text: '', attachments: [attachment] }],
      params: '{}',
    }))).toEqual([{ id: '7', author: 'you', text: '', atMs: 50, attachments: [attachment] }]);

    expect(transcriptRowToMessages(item({
      item_type: 'userMessage',
      input_segments: [{ presentation: 'user', text: '', attachments: [] }],
      params: '{}',
    }))).toEqual([]);
  });

  it('carries a segment\'s images alongside its words', () => {
    const attachment = {
      id: 'b.png', contentType: 'image/png', size: 3,
      url: '/api/cards/card/planner/attachments/b.png',
    };
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage',
      input_segments: [{ presentation: 'user', text: 'User says:\nlook', attachments: [attachment] }],
      params: '{}',
    }))).toEqual([{ id: '7', author: 'you', text: 'look', atMs: 50, attachments: [attachment] }]);
  });

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
      item_type: 'userMessage', input_segments: [{ presentation: inputPresentation, text, attachments: [] }],
      params: '{broken upstream frame',
    }))).toEqual([{ id: '7', author: 'system', label, text, atMs: 50 }]);
  });

  it('uses ordered segments instead of the diff-prefixed flattened echo', () => {
    const flattened = '## Track state changes since your last turn\nchanged\n\n---\n\nnew wording';
    expect(transcriptRowToMessages(item({
      item_type: 'userMessage',
      input_segments: [{ presentation: 'system_report_edited', text: 'new wording', attachments: [] }],
      params: JSON.stringify({
        completedAtMs: 99, item: { content: [{ type: 'text', text: flattened }] },
      }),
    }))).toEqual([{
      id: '7', author: 'system', label: 'Report edited', text: 'new wording', atMs: 99,
    }]);
  });

  it('never infers system authorship from English text', () => {
    const text = 'A dispatched task completed, according to the user';
    for (const inputSegments of [[{ presentation: 'user' as const, text, attachments: [] }], undefined]) {
      expect(transcriptRowToMessages(item({
        item_type: 'userMessage', input_segments: inputSegments,
        params: JSON.stringify({ item: { content: [{ type: 'text', text }] } }),
      }))).toMatchObject([{ author: 'you', text }]);
    }
  });

  it.each([
    [
      [
        { presentation: 'system_report_edited' as const, text: 'report changed', attachments: [] },
        { presentation: 'user' as const, text: 'User says:\nhello', attachments: [] },
      ],
      ['system', 'you'],
      ['report changed', 'hello'],
    ],
    [
      [
        { presentation: 'user' as const, text: 'User says:\nhello', attachments: [] },
        { presentation: 'system_task_completed' as const, text: 'task completed', attachments: [] },
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
    id: 7, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
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
  /** One word per entry: what an activity did, how a turn ended, or what was said. */
  const line = (entry: TranscriptEntry): string =>
    entry.author === 'activity' ? entry.verb : entry.author === 'turn' ? entry.status : entry.text;
  const row = (id: number, itemType: string, method: string, item: unknown, uuid = `u${id}`): HarnessItem => ({
    id, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn',
    item_uuid: uuid, item_type: itemType, method,
    params: JSON.stringify({ completedAtMs: 1000 + id, item }), created_at_ms: 1000 + id,
  });

  it('pairs started with completed into one line, in the started position', () => {
    const entries = buildTranscript([
      row(1, 'commandExecution', 'item/started', { command: 'ls', type: 'commandExecution' }),
      row(2, 'agentMessage', 'item/completed', { text: 'done', type: 'agentMessage' }, 'u-msg'),
      row(3, 'commandExecution', 'item/completed', { command: 'ls', exitCode: 0 }, 'u1'),
    ]);
    expect(entries.map(line))
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
    expect(buildTranscript(answered).map(line))
      .toEqual(['hi']);
  });

  it('orders by the wire id, not by arrival', () => {
    const entries = buildTranscript([
      row(9, 'agentMessage', 'item/completed', { text: 'second' }, 'u9'),
      row(4, 'userMessage', 'item/completed', { content: [{ text: 'first' }] }, 'u4'),
    ]);
    expect(entries.map(line))
      .toEqual(['first', 'second']);
  });

  it('expands one mixed user-message row into ordered transcript entries', () => {
    const mixed = {
      ...row(4, 'userMessage', 'item/completed', { content: [{ text: 'flattened' }] }, 'u4'),
      input_segments: [
        { presentation: 'system_report_edited' as const, text: 'report changed', attachments: [] },
        { presentation: 'user' as const, text: 'User says:\nhello', attachments: [] },
        { presentation: 'system_task_completed' as const, text: 'task completed', attachments: [] },
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
      id: 2, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn',
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
    ]).map(line))
      .toEqual(['go', 'done']);
  });

  /* #1625 P1 — the third allowed method. A `turn/completed` row is one
     outcome entry, in row order, keyed by its own row; it neither pairs with
     nor overwrites anything, and a `completed` one is kept (as the anchor a
     later slice groups by) rather than filtered here. */
  it('renders a turn/completed row as a turn outcome, in row order', () => {
    const outcomeRow = (id: number, turn: unknown): HarnessItem => ({
      id, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: `turn-${id}`,
      item_uuid: null, item_type: null, method: 'turn/completed',
      params: JSON.stringify(turn), created_at_ms: 1000 + id,
    });
    const entries = buildTranscript([
      row(1, 'userMessage', 'item/completed', { content: [{ text: 'go' }] }),
      row(2, 'agentMessage', 'item/completed', { text: 'done' }, 'u2'),
      outcomeRow(3, { id: 'turn-3', status: 'completed', durationMs: 12 }),
      row(4, 'userMessage', 'item/completed', { content: [{ text: 'again' }] }),
      outcomeRow(5, {
        id: 'turn-5', status: 'failed',
        error: { message: 'Context window exceeded', codexErrorInfo: 'contextWindowExceeded' },
      }),
    ]);
    expect(entries.map((entry) => entry.author)).toEqual(['you', 'agent', 'turn', 'you', 'turn']);
    expect(entries[2]).toEqual({
      id: 'outcome-3', author: 'turn', turnId: 'turn-3', status: 'completed', atMs: 1003,
    });
    expect(entries[4]).toEqual({
      id: 'outcome-5', author: 'turn', turnId: 'turn-5', status: 'failed',
      message: 'Context window exceeded', code: 'contextWindowExceeded', atMs: 1005,
    });
  });

  it('keeps a trailing thought when only a turn outcome follows it', () => {
    const entries = buildTranscript([
      row(1, 'reasoning', 'item/completed', { text: 'hmm' }),
      {
        id: 2, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn-2',
        item_uuid: null, item_type: null, method: 'turn/completed',
        params: JSON.stringify({ id: 'turn-2', status: 'failed', error: { message: 'boom' } }),
        created_at_ms: 1002,
      },
    ]);
    expect(entries.map((entry) => (entry.author === 'activity' ? entry.verb : entry.author)))
      .toEqual(['Thought', 'turn']);
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

describe('harnessItemToTurnOutcome', () => {
  const outcome = (params: unknown, overrides: Partial<HarnessItem> = {}): HarnessItem => ({
    id: 9, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: 'turn-9',
    item_uuid: null, item_type: null, method: 'turn/completed',
    params: typeof params === 'string' ? params : JSON.stringify(params), created_at_ms: 5000,
    ...overrides,
  });

  it.each(['completed', 'interrupted', 'failed'] as const)('parses a %s turn', (status) => {
    expect(harnessItemToTurnOutcome(outcome({ id: 'turn-9', status }))).toEqual({
      id: 'outcome-9', author: 'turn', turnId: 'turn-9', status, atMs: 5000,
    });
  });

  it('carries the error message and the bare codexErrorInfo token', () => {
    expect(harnessItemToTurnOutcome(outcome({
      id: 'turn-9', status: 'failed',
      error: { message: 'Usage limit hit', codexErrorInfo: 'usageLimitExceeded', additionalDetails: null },
    }))).toMatchObject({ status: 'failed', message: 'Usage limit hit', code: 'usageLimitExceeded' });
  });

  it('reduces the object form of codexErrorInfo to its single key', () => {
    expect(harnessItemToTurnOutcome(outcome({
      id: 'turn-9', status: 'failed',
      error: { message: 'gateway', codexErrorInfo: { httpConnectionFailed: { httpStatusCode: 502 } } },
    }))).toMatchObject({ status: 'failed', message: 'gateway', code: 'httpConnectionFailed' });
  });

  it('surfaces an unknown status as failed with the raw status, never dropping it', () => {
    expect(harnessItemToTurnOutcome(outcome({ id: 'turn-9', status: 'inProgress' }))).toEqual({
      id: 'outcome-9', author: 'turn', turnId: 'turn-9', status: 'failed', rawStatus: 'inProgress', atMs: 5000,
    });
  });

  it('takes the turn id from the row column before the params', () => {
    expect(harnessItemToTurnOutcome(outcome({ status: 'completed' }))?.turnId).toBe('turn-9');
    expect(harnessItemToTurnOutcome(outcome({ id: 'from-params', status: 'completed' }, { turn_id: null }))?.turnId)
      .toBe('from-params');
    expect(harnessItemToTurnOutcome(outcome({ status: 'completed' }, { turn_id: null }))).toBeNull();
  });

  it('is null for any other method and for params that are not a turn', () => {
    expect(harnessItemToTurnOutcome(outcome({ id: 'turn-9', status: 'failed' }, { method: 'item/completed' }))).toBeNull();
    expect(harnessItemToTurnOutcome(outcome('not json'))).toBeNull();
    expect(harnessItemToTurnOutcome(outcome({ id: 'turn-9' }))).toBeNull();
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

/*
 * #1505 PR4 — the classifier behind every message the queue region shows after
 * a refused write. The two refusals call for opposite next moves, so collapsing
 * them into one "it did not work" is the failure this exists to prevent: a
 * stale entry can be rewritten against the revision that beat you, a gone one
 * cannot be rewritten at all.
 */
describe('plannerQueueWriteFailure', () => {
  const stale = (body: unknown) => plannerQueueWriteFailure(
    { kind: 'http' as const, status: 409, code: 'planner_input_stale', message: 'stale', body },
    'fallback',
  );

  it('reads the winning text and revision out of a 409', () => {
    expect(stale({
      error: 'stale', code: 'planner_input_stale', entry_id: 'e1', text: 'what won', rev: 6,
    })).toEqual({ kind: 'stale', text: 'what won', rev: 6 });
  });

  /* A 409 whose body cannot be read is NOT reported as stale: there is no text
     and no revision to show, and inventing either is worse than saying the
     write failed. */
  it('falls back to a plain failure when a 409 body is unreadable', () => {
    expect(stale({ code: 'planner_input_stale' })).toEqual({ kind: 'failed', message: 'fallback' });
    expect(stale('not an object')).toEqual({ kind: 'failed', message: 'fallback' });
  });

  it('reads a 404 as the entry having left the queue', () => {
    expect(plannerQueueWriteFailure(
      { kind: 'http', status: 404, code: 'not_found', message: 'gone', body: null }, 'fallback',
    )).toEqual({ kind: 'gone' });
  });

  it('reports anything else as an unexplained failure', () => {
    expect(plannerQueueWriteFailure(
      { kind: 'http', status: 500, code: 'internal', message: 'boom', body: null }, 'fallback',
    )).toEqual({ kind: 'failed', message: 'fallback' });
    expect(plannerQueueWriteFailure(null, 'fallback')).toEqual({ kind: 'failed', message: 'fallback' });
  });
});

/*
 * #1505 S4 review — rapid picks were last-COMMITTER-wins, not
 * latest-CHOICE-wins. `PUT /planner/model` consults codex's catalog before it
 * writes, so two requests fired back to back can land in the other order and
 * an earlier choice quietly overwrites the person's latest one.
 */
describe('createSerialWriter', () => {
  function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((r) => { resolve = r; });
    return { promise, resolve };
  }

  it('never has two writes in flight at once', async () => {
    const gates = [deferred<string>(), deferred<string>()];
    const started: string[] = [];
    let live = 0;
    let maxLive = 0;
    const write = createSerialWriter(async (name: string) => {
      started.push(name);
      live += 1;
      maxLive = Math.max(maxLive, live);
      const value = await gates[started.length - 1].promise;
      live -= 1;
      return value;
    });

    const first = write('a');
    const second = write('b');
    expect(started).toEqual(['a']);
    gates[0].resolve('a-done');
    await Promise.resolve();
    gates[1].resolve('b-done');
    await Promise.all([first, second]);

    expect(started).toEqual(['a', 'b']);
    expect(maxLive).toBe(1);
  });

  /*
   * The defect itself. Without serialisation the second request skips the
   * first one's catalog round trip, commits first, and then the first commits
   * over it — restoring a choice the person had already moved on from.
   */
  it('lets the last intent be the one that lands', async () => {
    const gate = deferred<string>();
    const committed: string[] = [];
    const write = createSerialWriter(async (name: string) => {
      if (name === 'a') await gate.promise;
      committed.push(name);
      return name;
    });

    const first = write('a');
    const second = write('b');
    gate.resolve('go');
    await Promise.all([first, second]);

    expect(committed).toEqual(['a', 'b']);
    expect(committed.at(-1)).toBe('b');
  });

  /*
   * Four fast clicks are two requests, not four: an intent nobody can still
   * see the effect of is not worth a round trip, and replaying it would put
   * the store through values the person never ended on.
   */
  it('collapses superseded intents instead of replaying every one', async () => {
    const gate = deferred<string>();
    const committed: string[] = [];
    const write = createSerialWriter(async (name: string) => {
      if (name === 'a') await gate.promise;
      committed.push(name);
      return name;
    });

    const all = [write('a'), write('b'), write('c'), write('d')];
    gate.resolve('go');
    await Promise.all(all);

    expect(committed).toEqual(['a', 'd']);
  });

  /*
   * The rejection path, which the first cut of this function got exactly
   * backwards. Every other test here uses a write that only ever resolves,
   * which is why nothing caught it.
   */
  it('still sends the intent that superseded a failed write', async () => {
    const gate = deferred<string>();
    const attempted: string[] = [];
    const write = createSerialWriter(async (name: string) => {
      attempted.push(name);
      if (name === 'a') {
        await gate.promise;
        throw new Error('offline');
      }
      return name;
    });

    const chain = write('a');
    void write('b');
    gate.resolve('go');
    await expect(chain).resolves.toBe('b');
    expect(attempted).toEqual(['a', 'b']);
  });

  /*
   * And the second half: a failure must not leave the queue armed for a LATER,
   * unrelated call to replay. Pre-fix, `queuedIsSet` survived the rejection and
   * `c` was followed by a resurrected `b` — the server ended on `b` while the
   * reader had chosen `c`.
   */
  it('cannot resurrect a stranded intent onto a later write', async () => {
    const gate = deferred<string>();
    const committed: string[] = [];
    let failNext = true;
    const write = createSerialWriter(async (name: string) => {
      if (name === 'a') {
        await gate.promise;
        if (failNext) throw new Error('offline');
      }
      committed.push(name);
      return name;
    });

    const chain = write('a');
    void write('b');
    gate.resolve('go');
    await chain.catch(() => undefined);
    failNext = false;

    await write('c');
    expect(committed).toEqual(['b', 'c']);
    expect(committed.at(-1)).toBe('c');
  });

  /*
   * A failure with nothing queued is the chain's own answer, and it must still
   * leave the writer usable.
   */
  it('reports a lone failure and stays usable afterwards', async () => {
    let fail = true;
    const write = createSerialWriter((name: string) =>
      fail ? Promise.reject(new Error('offline')) : Promise.resolve(name));

    await expect(write('a')).rejects.toThrow('offline');
    fail = false;
    await expect(write('b')).resolves.toBe('b');
  });

  it('accepts new work again after the queue drains', async () => {
    const committed: string[] = [];
    const write = createSerialWriter((name: string) => {
      committed.push(name);
      return Promise.resolve(name);
    });
    await write('a');
    await write('b');
    expect(committed).toEqual(['a', 'b']);
  });
});
