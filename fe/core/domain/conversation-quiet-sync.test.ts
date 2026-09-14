import { describe, expect, it } from 'vitest';

import {
  PLAN_LIST_TOOL, REPORT_DELETE_TOOL, REPORT_MOVE_TOOL, REPORT_READ_TOOLS, REPORT_TOOL_PREFIX,
  REPORT_WRITE_TOOLS, TRACK_TOOL_PREFIX, USER_NOTIFY_TOOL,
} from '../keys/mcp-tools.js';
import {
  REPORT_EDIT_AUTHORS, foldQuietSyncs, isNotifyTurn, isReportWriteTool, reportEditAuthor,
  type QuietSyncGroup, type ReportEditAuthor, type TranscriptBlock,
} from './conversation-quiet-sync.js';
import {
  SYSTEM_PRESENTATION_LABELS, buildTranscript,
  type ConversationActivity, type ConversationSystemEntry, type ConversationTurn,
  type ConversationTurnOutcome, type TranscriptEntry,
} from './conversation.js';

/** One persisted transcript row, as `buildTranscript` takes them. */
type Row = Parameters<typeof buildTranscript>[0][number];

const NOW = 1_760_000_000_000;

const DIFF_TEXT = 'The track report was edited (author = "user").\n'
  + 'Block-level diff follows; this is information, not an instruction to re-read.\n'
  + 'Blocks: 0 added, 0 removed, 1 modified (2 unchanged).\n';

/** A report-edit wake that was the whole of its batch — what the segment
 *  converter in `conversation.ts` marks `quiet` (see the `buildTranscript`
 *  cases below for the flag being derived from the persisted segments). */
function reportEdited(id: string, text = DIFF_TEXT): ConversationSystemEntry {
  return {
    id, author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text, atMs: NOW,
    quiet: true,
  };
}

function otherSystem(id: string): ConversationSystemEntry {
  return {
    id, author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_task_completed,
    text: 'Task completed.', atMs: NOW,
  };
}

function you(id: string, text = 'hello'): ConversationTurn {
  return { id, author: 'you', text, atMs: NOW };
}

function agent(id: string, text = 'I re-read the report.'): ConversationTurn {
  return { id, author: 'agent', text, atMs: NOW };
}

function notify(id: string, text = 'You changed the block I was writing.'): ConversationTurn {
  return { id, author: 'agent', text, atMs: NOW, origin: 'notify' };
}

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Read report', target: null, state: 'done',
    durationMs: null, detail: null, tool: REPORT_READ_TOOLS[0] ?? null, atMs: NOW, ...overrides,
  };
}

/** A report write that landed, as `buildTranscript` shapes a completed
 *  `calm.report.commit` row. */
function wrote(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return activity(id, { verb: 'Wrote report', tool: REPORT_WRITE_TOOLS[4] ?? null, ...overrides });
}

function outcome(id: string, status: ConversationTurnOutcome['status'] = 'completed'): ConversationTurnOutcome {
  return { id, author: 'turn', turnId: 'turn-1', status, atMs: NOW };
}

function ids(block: TranscriptBlock): string | readonly string[] {
  return block.kind === 'entry' ? block.entry.id : block.entries.map((entry) => entry.id);
}

function group(block: TranscriptBlock | undefined): QuietSyncGroup {
  if (block?.kind !== 'quiet-sync') throw new Error(`expected a quiet-sync group, got ${JSON.stringify(block)}`);
  return block;
}

describe('foldQuietSyncs', () => {
  /* A6 — a report-edit wake folds the whole turn after it. */
  it('folds a report-edit wake with the activity and replies that followed it', () => {
    const blocks = foldQuietSyncs([
      you('u1'), agent('a1'),
      reportEdited('s1'), activity('act1'), agent('a2'), outcome('o1'),
      you('u2'),
    ]);
    expect(blocks.map(ids)).toEqual([
      'u1', 'a1', ['s1', 'act1', 'a2', 'o1'], 'u2',
    ]);
    const sync = group(blocks[2]);
    expect(sync.id).toBe('s1');
    expect(sync.author).toBe('user');
    expect(sync.atMs).toBe(NOW);
    expect(sync.entries[0]?.author).toBe('system');
  });

  /* A6 — the reader speaking closes the fold; a turn the reader opened is
     never folded, whatever came before it. */
  it('does not fold a turn the user opened, and closes a fold at the user turn', () => {
    const blocks = foldQuietSyncs([
      reportEdited('s1'), activity('act1'),
      you('u1'), activity('act2'), agent('a1'),
    ]);
    expect(blocks.map(ids)).toEqual([['s1', 'act1'], 'u1', 'act2', 'a1']);
    expect(blocks.slice(1).every((block) => block.kind === 'entry')).toBe(true);
  });

  /* A6 — a `calm.user.notify` is speech: lifted out of the fold, after it. */
  it('lifts a notify turn out of the fold and keeps the rest folded', () => {
    const blocks = foldQuietSyncs([
      reportEdited('s1'), activity('act1'), notify('n1'), activity('act2'), agent('a1'),
      you('u1'),
    ]);
    expect(blocks.map(ids)).toEqual([['s1', 'act1', 'act2', 'a1'], 'n1', 'u1']);
    const lifted = blocks[1];
    expect(lifted?.kind).toBe('entry');
    expect(lifted?.kind === 'entry' && isNotifyTurn(lifted.entry)).toBe(true);
    expect(lifted?.kind === 'entry' && lifted.entry.author).toBe('agent');
  });

  it('keeps an ordinary agent reply inside the fold — only a notify is lifted', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), agent('a1')]);
    expect(blocks.map(ids)).toEqual([['s1', 'a1']]);
  });

  /* Two wakes are two folds, never one (the issue's "各自折叠，不合并"). */
  it('folds consecutive report-edit wakes separately and stops at any system entry', () => {
    const blocks = foldQuietSyncs([
      reportEdited('s1'), activity('act1'),
      reportEdited('s2'), agent('a1'),
      otherSystem('s3'), agent('a2'),
    ]);
    expect(blocks.map(ids)).toEqual([['s1', 'act1'], ['s2', 'a1'], 's3', 'a2']);
  });

  it('leaves a transcript without report-edit wakes untouched', () => {
    const entries: readonly TranscriptEntry[] = [you('u1'), activity('act1'), agent('a1'), otherSystem('s1')];
    const blocks = foldQuietSyncs(entries);
    expect(blocks.map(ids)).toEqual(['u1', 'act1', 'a1', 's1']);
    expect(blocks.map((block) => (block.kind === 'entry' ? block.entry : null))).toEqual(entries);
  });

  it('keeps a notify outside any sync as an ordinary entry', () => {
    const blocks = foldQuietSyncs([you('u1'), notify('n1')]);
    expect(blocks.map(ids)).toEqual(['u1', 'n1']);
  });

  /* Round-4 N2 — a sync that ended badly must be seen: the failed or
     interrupted outcome is lifted out after the fold, a completed one stays
     inside (it renders as nothing). */
  it('lifts a failed or interrupted outcome out of the fold, after it', () => {
    const failed = foldQuietSyncs([reportEdited('s1'), activity('act1'), outcome('o1', 'failed'), you('u1')]);
    expect(failed.map(ids)).toEqual([['s1', 'act1'], 'o1', 'u1']);
    const stopped = foldQuietSyncs([reportEdited('s1'), agent('a1'), outcome('o1', 'interrupted')]);
    expect(stopped.map(ids)).toEqual([['s1', 'a1'], 'o1']);
    const fine = foldQuietSyncs([reportEdited('s1'), agent('a1'), outcome('o1', 'completed')]);
    expect(fine.map(ids)).toEqual([['s1', 'a1', 'o1']]);
  });

  /* A wake that is not marked `quiet` is a plain system line: nothing after
     it is folded, whatever its label says. */
  it('does not fold a report-edit entry that is not marked quiet', () => {
    const unmarked: ConversationSystemEntry = {
      id: 's1', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text: DIFF_TEXT, atMs: NOW,
    };
    const blocks = foldQuietSyncs([unmarked, activity('act1'), agent('a1')]);
    expect(blocks.map(ids)).toEqual(['s1', 'act1', 'a1']);
    expect(blocks.every((block) => block.kind === 'entry')).toBe(true);
  });
});

/* #1678 A4 — what the sync did, on the group, so the line can say it. */
describe('foldQuietSyncs outcome', () => {
  it('is accepted when the turn completed with reads only and nothing said', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), activity('act1'), agent('a1'), outcome('o1')]);
    expect(group(blocks[0]).outcome).toBe('accepted');
    /* No activity at all is the same verdict: the planner looked and left. */
    expect(group(foldQuietSyncs([reportEdited('s1'), outcome('o1')])[0]).outcome).toBe('accepted');
  });

  it('is updated when a report write landed in the turn', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), activity('act1'), wrote('w1'), outcome('o1')]);
    expect(group(blocks[0]).outcome).toBe('updated');
    /* Every report tool that is not a read counts, move and delete included. */
    for (const tool of [...REPORT_WRITE_TOOLS, REPORT_MOVE_TOOL, REPORT_DELETE_TOOL]) {
      const one = foldQuietSyncs([reportEdited('s1'), wrote('w1', { tool }), outcome('o1')]);
      expect(group(one[0]).outcome, tool).toBe('updated');
    }
    /* And a write the planner said something about is still an update. */
    const spoken = foldQuietSyncs([reportEdited('s1'), wrote('w1'), notify('n1'), outcome('o1')]);
    expect(group(spoken[0]).outcome).toBe('updated');
  });

  it('is null while the turn has not completed, after a bad ending, or when the planner spoke', () => {
    expect(group(foldQuietSyncs([reportEdited('s1'), activity('act1')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), wrote('w1')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), wrote('w1'), outcome('o1', 'failed')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), outcome('o1', 'interrupted')])[0]).outcome).toBeNull();
    /* A notify without a write: the bubble under the line is the outcome. */
    expect(group(foldQuietSyncs([reportEdited('s1'), notify('n1'), outcome('o1')])[0]).outcome).toBeNull();
  });

  it('does not count a refused or still-running write, nor a read, nor a non-report tool', () => {
    const refused = foldQuietSyncs([reportEdited('s1'), wrote('w1', { state: 'failed' }), outcome('o1')]);
    expect(group(refused[0]).outcome).toBe('accepted');
    const running = foldQuietSyncs([reportEdited('s1'), wrote('w1', { state: 'running' }), outcome('o1')]);
    expect(group(running[0]).outcome).toBe('accepted');
    const reads = foldQuietSyncs([
      reportEdited('s1'), ...REPORT_READ_TOOLS.map((tool, index) => activity(`r${index}`, { tool })), outcome('o1'),
    ]);
    expect(group(reads[0]).outcome).toBe('accepted');
    const other = foldQuietSyncs([reportEdited('s1'), activity('t1', { tool: PLAN_LIST_TOOL }), outcome('o1')]);
    expect(group(other[0]).outcome).toBe('accepted');
    const shell = foldQuietSyncs([reportEdited('s1'), activity('sh', { verb: 'Ran', tool: null }), outcome('o1')]);
    expect(group(shell[0]).outcome).toBe('accepted');
  });

  /* Fail-closed on the name: a report tool nobody has listed is a change. */
  it('treats any unlisted report tool as a write', () => {
    expect(isReportWriteTool(`${REPORT_TOOL_PREFIX}blocks.something_new`)).toBe(true);
    for (const tool of REPORT_READ_TOOLS) expect(isReportWriteTool(tool), tool).toBe(false);
    for (const tool of REPORT_WRITE_TOOLS) expect(isReportWriteTool(tool), tool).toBe(true);
    expect(isReportWriteTool(USER_NOTIFY_TOOL)).toBe(false);
    expect(isReportWriteTool(`${TRACK_TOOL_PREFIX}cat`)).toBe(false);
    /* The prefix is matched whole: a near miss is not a report tool. */
    expect(isReportWriteTool(`${REPORT_TOOL_PREFIX.slice(0, -1)}ing.x`)).toBe(false);
  });

  /* Driven through `buildTranscript` from persisted rows, so the `tool` the
     verdict reads is the one the row converter writes, not one a fixture set. */
  it('reads the verdict off persisted rows', () => {
    type Row = Parameters<typeof buildTranscript>[0][number];
    const row = (id: number, overrides: Partial<Row>): Row => ({
      id, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
      turn_id: 'turn', item_uuid: `item-${id}`, item_type: null, method: 'item/completed',
      params: '{}', created_at_ms: NOW + id, ...overrides,
    });
    const wake = row(1, {
      item_type: 'userMessage',
      input_segments: [{ presentation: 'system_report_edited', text: DIFF_TEXT, attachments: [] }],
      params: JSON.stringify({ completedAtMs: NOW + 1, item: { content: [] } }),
    });
    const call = (id: number, tool: string): Row => row(id, {
      item_type: 'mcpToolCall',
      params: JSON.stringify({ completedAtMs: NOW + id, item: { tool, status: 'completed' } }),
    });
    const done = row(9, {
      item_type: null, method: 'turn/completed',
      params: JSON.stringify({ id: 'turn', status: 'completed' }),
    });
    const read = foldQuietSyncs(buildTranscript([wake, call(2, REPORT_READ_TOOLS[0] ?? ''), done]));
    expect(group(read[0]).outcome).toBe('accepted');
    const write = foldQuietSyncs(buildTranscript([wake, call(2, REPORT_READ_TOOLS[0] ?? ''), call(3, REPORT_WRITE_TOOLS[4] ?? ''), done]));
    expect(group(write[0]).outcome).toBe('updated');
  });
});

/* Round-4 M1 — the fold mirrors the kernel's batch rule
   (`queue_is_only_report_edits`): a turn is a quiet sync only when its
   batch held nothing but report edits. Driven through `buildTranscript`
   from persisted rows, because the `quiet` mark is derived there and a
   fixture that set it by hand would prove nothing about that derivation. */
describe('foldQuietSyncs over persisted batches', () => {
  type Segment = NonNullable<Row['input_segments']>[number];
  const userSays = (text: string): Segment => ({ presentation: 'user', text: `User says:\n${text}`, attachments: [] });
  const edited = (): Segment => ({ presentation: 'system_report_edited', text: DIFF_TEXT, attachments: [] });
  const taskDone = (): Segment => ({ presentation: 'system_task_completed', text: 'Task completed.', attachments: [] });
  const row = (id: number, overrides: Partial<Row>): Row => ({
    id, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', item_uuid: `item-${id}`, item_type: null, method: 'item/completed',
    params: '{}', created_at_ms: NOW + id, ...overrides,
  });
  const batch = (id: number, segments: Segment[]): Row => row(id, {
    item_type: 'userMessage', input_segments: segments,
    params: JSON.stringify({ completedAtMs: NOW + id, item: { content: [] } }),
  });
  const reply = (id: number, text: string): Row => row(id, {
    item_type: 'agentMessage', params: JSON.stringify({ completedAtMs: NOW + id, item: { text } }),
  });
  const action = (id: number): Row => row(id, {
    item_type: 'mcpToolCall',
    params: JSON.stringify({ completedAtMs: NOW + id, item: { tool: REPORT_READ_TOOLS[0], status: 'completed' } }),
  });

  it('does not fold a batch of a user message and a report edit — the reply is to the user', () => {
    const entries = buildTranscript([batch(1, [userSays('please add a risks section'), edited()]), action(2), reply(3, 'Added.')]);
    expect(entries.map((entry) => entry.id)).toEqual(['1:0', '1:1', 'activity-item-2', '3']);
    expect(entries[1]).toMatchObject({ author: 'system', label: 'Report edited' });
    expect((entries[1] as { quiet?: true }).quiet).toBeUndefined();
    const blocks = foldQuietSyncs(entries);
    expect(blocks.map(ids)).toEqual(['1:0', '1:1', 'activity-item-2', '3']);
    expect(blocks.every((block) => block.kind === 'entry')).toBe(true);
  });

  it('does not fold a batch of a task event and a report edit, whichever comes first', () => {
    const eventFirst = buildTranscript([batch(1, [taskDone(), edited()]), reply(2, 'The task landed; I noted it.')]);
    expect(foldQuietSyncs(eventFirst).map(ids)).toEqual(['1:0', '1:1', '2']);
    const editFirst = buildTranscript([batch(1, [edited(), taskDone()]), reply(2, 'The task landed; I noted it.')]);
    expect(foldQuietSyncs(editFirst).map(ids)).toEqual(['1:0', '1:1', '2']);
  });

  it('folds a batch that is nothing but the report edit', () => {
    const entries = buildTranscript([batch(1, [edited()]), action(2), reply(3, 'I re-read the report.')]);
    expect(entries[0]).toMatchObject({ id: '1', author: 'system', label: 'Report edited', quiet: true });
    expect(foldQuietSyncs(entries).map(ids)).toEqual([['1', 'activity-item-2', '3']]);
  });
});

describe('reportEditAuthor', () => {
  it('reads the author the kernel named on the first line', () => {
    expect(reportEditAuthor(DIFF_TEXT)).toBe('user');
    expect(reportEditAuthor('The track report was edited (author = "plugin"). Re-read the track state.')).toBe('plugin');
    expect(reportEditAuthor('The track report was edited (author = "assistant").\nmore')).toBe('assistant');
  });

  it('answers null for the pre-#1252 sentence and for an unknown spelling', () => {
    expect(reportEditAuthor('The user edited the track report. Re-read the track state.')).toBeNull();
    expect(reportEditAuthor('The track report was edited (author = "kernel").')).toBeNull();
    expect(reportEditAuthor('')).toBeNull();
    // Only the first line counts: a diff excerpt quoting the spelling is not the author.
    expect(reportEditAuthor('The user edited the track report.\n+(author = "plugin")')).toBeNull();
  });

  /* Both directions of the author vocabulary: every listed author parses,
     and nothing parses that is not listed. */
  it('accepts exactly the listed authors', () => {
    const parsed = new Set(REPORT_EDIT_AUTHORS.map((author: ReportEditAuthor) =>
      reportEditAuthor(`The track report was edited (author = "${author}").`)));
    expect([...parsed].sort()).toEqual([...REPORT_EDIT_AUTHORS].sort());
    expect(Object.isFrozen(REPORT_EDIT_AUTHORS)).toBe(true);
  });
});

describe('user-notify rows', () => {
  const row = (overrides: Partial<Row>): Row => ({
    id: 40, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'exec-notify-1', item_type: 'mcpToolCall', method: 'item/completed',
    params: '{}', created_at_ms: NOW, ...overrides,
  });
  /* The persisted shape, as the production transcript table has it for
     every `mcpToolCall` row: `params.item.arguments` on `item/started` and
     `item/completed` alike. */
  const notifyParams = (extra: Record<string, unknown> = {}) => JSON.stringify({
    completedAtMs: NOW + 5,
    item: {
      appContext: null, arguments: { text: '  Heads up: you edited the block I was writing.  ' },
      durationMs: 3, error: null, id: 'exec-notify-1', pluginId: null, readOnlyHint: false,
      result: { content: [{ text: '{"ok":true}', type: 'text' }], structuredContent: { ok: true } },
      server: 'calm', status: 'completed', tool: USER_NOTIFY_TOOL, type: 'mcpToolCall', ...extra,
    },
  });

  it('reads a notify call as an agent turn with the text verbatim and trimmed', () => {
    const turns = buildTranscript([row({ params: notifyParams() })]);
    expect(turns).toEqual([{
      id: 'notify-exec-notify-1', author: 'agent',
      text: 'Heads up: you edited the block I was writing.', atMs: NOW + 5, origin: 'notify',
    }]);
  });

  const started = () => row({
    id: 39, method: 'item/started',
    params: JSON.stringify({
      item: { arguments: { text: 'Heads up' }, status: 'inProgress', tool: USER_NOTIFY_TOOL, type: 'mcpToolCall' },
    }),
  });

  /* Round-4 N1 — the started row is the running line, and the successful
     completed row becomes the bubble in that line's place: one entry. */
  it('draws item/started as a running line that the completed row replaces in place', () => {
    expect(buildTranscript([started()])).toMatchObject([{ author: 'activity', state: 'running', verb: 'Calling' }]);
    const completed = row({ params: notifyParams({ arguments: { text: 'Heads up' } }) });
    const entries = buildTranscript([completed, started()]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ id: 'notify-exec-notify-1', author: 'agent', text: 'Heads up', atMs: NOW + 5 });
  });

  /* Round-4 N1 — a call the kernel refused (blank, over 2000 characters) is
     not something the agent said: no bubble, one failed line with the reason. */
  it('draws a refused notify as a failed line, not a bubble', () => {
    const refused = row({
      params: notifyParams({
        arguments: { text: 'x'.repeat(2001) },
        error: { message: 'tool call error: tool call failed\n\nCaused by:\n    Mcp error: -32602: text must be at most 2000 characters' },
        result: null, status: 'failed',
      }),
    });
    const entries = buildTranscript([started(), refused]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      author: 'activity', state: 'failed', verb: 'Called',
      detail: 'Mcp error: -32602: text must be at most 2000 characters',
    });
    expect(entries.some(isNotifyTurn)).toBe(false);
    /* `status: 'failed'` alone, without an error member, is a refusal too. */
    const statusOnly = row({ params: notifyParams({ status: 'failed' }) });
    expect(buildTranscript([statusOnly]).map((entry) => entry.author)).toEqual(['activity']);
  });

  it('leaves every other tool call an activity line', () => {
    const other = row({ params: notifyParams({ tool: REPORT_READ_TOOLS[0] }) });
    const [entry] = buildTranscript([other]);
    expect(entry?.author).toBe('activity');
    expect(entry).toMatchObject({ verb: 'Read report' });
  });

  it('is not a message without text', () => {
    const blank = row({ params: notifyParams({ arguments: { text: '   ' } }) });
    expect(buildTranscript([blank]).map((entry) => entry.author)).toEqual(['activity']);
    const missing = row({ params: notifyParams({ arguments: {} }) });
    expect(buildTranscript([missing]).map((entry) => entry.author)).toEqual(['activity']);
  });
});
