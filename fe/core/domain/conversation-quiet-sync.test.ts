import { describe, expect, it } from 'vitest';

import {
  TASK_LS_TOOL, REPORT_DELETE_TOOL, REPORT_READ_TOOLS, REPORT_TOOL_PREFIX,
  REPORT_WRITE_TOOLS, TRACK_TOOL_PREFIX, USER_ASK_TOOL,
} from '../keys/mcp-tools.js';
import {
  REPORT_EDIT_AUTHORS, foldQuietSyncs, isAskTurn, isReportWriteTool, reportEditAuthor,
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

/** A report-edit wake that was the whole of its batch, marked `quiet`. */
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

function ask(id: string, text = 'You changed the block I was writing. Keep yours or mine?'): ConversationTurn {
  return { id, author: 'agent', text, atMs: NOW, origin: 'ask' };
}

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Read report', target: null, state: 'done',
    durationMs: null, detail: null, tool: REPORT_READ_TOOLS[0] ?? null, atMs: NOW, ...overrides,
  };
}

/** A completed `neige_report_commit` row. */
function wrote(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return activity(id, { verb: 'Wrote report', tool: REPORT_WRITE_TOOLS[2] ?? null, ...overrides });
}

function outcome(id: string, status: ConversationTurnOutcome['status'] = 'completed'): ConversationTurnOutcome {
  return { id, author: 'turn', elapsedMs: null, turnId: 'turn-1', status, atMs: NOW };
}

function ids(block: TranscriptBlock): string | readonly string[] {
  return block.kind === 'entry' ? block.entry.id : block.entries.map((entry) => entry.id);
}

function group(block: TranscriptBlock | undefined): QuietSyncGroup {
  if (block?.kind !== 'quiet-sync') throw new Error(`expected a quiet-sync group, got ${JSON.stringify(block)}`);
  return block;
}

describe('foldQuietSyncs', () => {
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

  it('does not fold a turn the user opened, and closes a fold at the user turn', () => {
    const blocks = foldQuietSyncs([
      reportEdited('s1'), activity('act1'),
      you('u1'), activity('act2'), agent('a1'),
    ]);
    expect(blocks.map(ids)).toEqual([['s1', 'act1'], 'u1', 'act2', 'a1']);
    expect(blocks.slice(1).every((block) => block.kind === 'entry')).toBe(true);
  });

  it('lifts an ask turn out of the fold and keeps the rest folded', () => {
    const blocks = foldQuietSyncs([
      reportEdited('s1'), activity('act1'), ask('n1'), activity('act2'), agent('a1'),
      you('u1'),
    ]);
    expect(blocks.map(ids)).toEqual([['s1', 'act1', 'act2', 'a1'], 'n1', 'u1']);
    const lifted = blocks[1];
    expect(lifted?.kind).toBe('entry');
    expect(lifted?.kind === 'entry' && isAskTurn(lifted.entry)).toBe(true);
    expect(lifted?.kind === 'entry' && lifted.entry.author).toBe('agent');
  });

  it('keeps an ordinary agent reply inside the fold — only an ask is lifted', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), agent('a1')]);
    expect(blocks.map(ids)).toEqual([['s1', 'a1']]);
  });

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

  it('keeps an ask outside any sync as an ordinary entry', () => {
    const blocks = foldQuietSyncs([you('u1'), ask('n1')]);
    expect(blocks.map(ids)).toEqual(['u1', 'n1']);
  });

  it('lifts a failed or interrupted outcome out of the fold, after it', () => {
    const failed = foldQuietSyncs([reportEdited('s1'), activity('act1'), outcome('o1', 'failed'), you('u1')]);
    expect(failed.map(ids)).toEqual([['s1', 'act1'], 'o1', 'u1']);
    const stopped = foldQuietSyncs([reportEdited('s1'), agent('a1'), outcome('o1', 'interrupted')]);
    expect(stopped.map(ids)).toEqual([['s1', 'a1'], 'o1']);
    const fine = foldQuietSyncs([reportEdited('s1'), agent('a1'), outcome('o1', 'completed')]);
    expect(fine.map(ids)).toEqual([['s1', 'a1', 'o1']]);
  });

  it('does not fold a report-edit entry that is not marked quiet', () => {
    const unmarked: ConversationSystemEntry = {
      id: 's1', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text: DIFF_TEXT, atMs: NOW,
    };
    const blocks = foldQuietSyncs([unmarked, activity('act1'), agent('a1')]);
    expect(blocks.map(ids)).toEqual(['s1', 'act1', 'a1']);
    expect(blocks.every((block) => block.kind === 'entry')).toBe(true);
  });
});

describe('foldQuietSyncs outcome', () => {
  it('is accepted when the turn completed with reads only and nothing said', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), activity('act1'), agent('a1'), outcome('o1')]);
    expect(group(blocks[0]).outcome).toBe('accepted');
    expect(group(foldQuietSyncs([reportEdited('s1'), outcome('o1')])[0]).outcome).toBe('accepted');
  });

  it('is updated when a report write landed in the turn', () => {
    const blocks = foldQuietSyncs([reportEdited('s1'), activity('act1'), wrote('w1'), outcome('o1')]);
    expect(group(blocks[0]).outcome).toBe('updated');
    for (const tool of [...REPORT_WRITE_TOOLS, REPORT_DELETE_TOOL]) {
      const one = foldQuietSyncs([reportEdited('s1'), wrote('w1', { tool }), outcome('o1')]);
      expect(group(one[0]).outcome, tool).toBe('updated');
    }
    const spoken = foldQuietSyncs([reportEdited('s1'), wrote('w1'), ask('n1'), outcome('o1')]);
    expect(group(spoken[0]).outcome).toBe('updated');
  });

  it('is null while the turn has not completed, after a bad ending, or when the planner spoke', () => {
    expect(group(foldQuietSyncs([reportEdited('s1'), activity('act1')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), wrote('w1')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), wrote('w1'), outcome('o1', 'failed')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), outcome('o1', 'interrupted')])[0]).outcome).toBeNull();
    expect(group(foldQuietSyncs([reportEdited('s1'), ask('n1'), outcome('o1')])[0]).outcome).toBeNull();
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
    const other = foldQuietSyncs([reportEdited('s1'), activity('t1', { tool: TASK_LS_TOOL }), outcome('o1')]);
    expect(group(other[0]).outcome).toBe('accepted');
    const shell = foldQuietSyncs([reportEdited('s1'), activity('sh', { verb: 'Ran', tool: null }), outcome('o1')]);
    expect(group(shell[0]).outcome).toBe('accepted');
  });

  it('treats any unlisted report tool as a write', () => {
    expect(isReportWriteTool(`${REPORT_TOOL_PREFIX}blocks.something_new`)).toBe(true);
    for (const tool of REPORT_READ_TOOLS) expect(isReportWriteTool(tool), tool).toBe(false);
    for (const tool of REPORT_WRITE_TOOLS) expect(isReportWriteTool(tool), tool).toBe(true);
    expect(isReportWriteTool(USER_ASK_TOOL)).toBe(false);
    expect(isReportWriteTool(`${TRACK_TOOL_PREFIX}cat`)).toBe(false);
    expect(isReportWriteTool(`${REPORT_TOOL_PREFIX.slice(0, -1)}ing.x`)).toBe(false);
  });

  it('reads the verdict off persisted rows', () => {
    type Row = Parameters<typeof buildTranscript>[0][number];
    const row = (id: number, overrides: Partial<Row>): Row => ({
      id, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
      turn_id: 'turn', turn_error_text: null, item_uuid: `item-${id}`, item_type: null, method: 'item/completed',
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
    const write = foldQuietSyncs(buildTranscript([wake, call(2, REPORT_READ_TOOLS[0] ?? ''), call(3, REPORT_WRITE_TOOLS[2] ?? ''), done]));
    expect(group(write[0]).outcome).toBe('updated');
  });
});

/* Driven through `buildTranscript` from persisted rows, because the `quiet` mark is derived there. */
describe('foldQuietSyncs over persisted batches', () => {
  type Segment = NonNullable<Row['input_segments']>[number];
  const userSays = (text: string): Segment => ({ presentation: 'user', text: `User says:\n${text}`, attachments: [] });
  const edited = (): Segment => ({ presentation: 'system_report_edited', text: DIFF_TEXT, attachments: [] });
  const taskDone = (): Segment => ({ presentation: 'system_task_completed', text: 'Task completed.', attachments: [] });
  const row = (id: number, overrides: Partial<Row>): Row => ({
    id, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', turn_error_text: null, item_uuid: `item-${id}`, item_type: null, method: 'item/completed',
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
    expect(reportEditAuthor('The track report was edited (author = "plugin"). Re-read the track status.')).toBe('plugin');
    expect(reportEditAuthor('The track report was edited (author = "assistant").\nmore')).toBe('assistant');
  });

  it('answers null for the pre-#1252 sentence and for an unknown spelling', () => {
    expect(reportEditAuthor('The user edited the track report. Re-read the track status.')).toBeNull();
    expect(reportEditAuthor('The track report was edited (author = "kernel").')).toBeNull();
    expect(reportEditAuthor('')).toBeNull();
    expect(reportEditAuthor('The user edited the track report.\n+(author = "plugin")')).toBeNull();
  });

  it('accepts exactly the listed authors', () => {
    const parsed = new Set(REPORT_EDIT_AUTHORS.map((author: ReportEditAuthor) =>
      reportEditAuthor(`The track report was edited (author = "${author}").`)));
    expect([...parsed].sort()).toEqual([...REPORT_EDIT_AUTHORS].sort());
    expect(Object.isFrozen(REPORT_EDIT_AUTHORS)).toBe(true);
  });
});

describe('user-ask rows', () => {
  const row = (overrides: Partial<Row>): Row => ({
    id: 40, worker_session_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
    turn_id: 'turn', turn_error_text: null, item_uuid: 'exec-ask-1', item_type: 'mcpToolCall', method: 'item/completed',
    params: '{}', created_at_ms: NOW, ...overrides,
  });
  /* The persisted shape: `params.item.arguments` on `item/started` and `item/completed` alike. */
  const questions = (...titles: string[]) => ({ questions: titles.map((title) => ({ title })) });
  const askParams = (extra: Record<string, unknown> = {}) => JSON.stringify({
    completedAtMs: NOW + 5,
    item: {
      appContext: null,
      arguments: { questions: [
        { title: '  You edited the block I was writing: keep yours?  ', options: [' Keep mine ', 'Keep yours'] },
        { title: 'Which branch?' },
      ] },
      durationMs: 3, error: null, id: 'exec-ask-1', pluginId: null, readOnlyHint: false,
      result: { content: [{ text: '{"ask_id":7}', type: 'text' }], structuredContent: { ask_id: 7 } },
      server: 'neige', status: 'completed', tool: USER_ASK_TOOL, type: 'mcpToolCall', ...extra,
    },
  });

  it('reads an ask call as an agent turn, one trimmed line per question with its options', () => {
    const turns = buildTranscript([row({ params: askParams() })]);
    expect(turns).toEqual([{
      id: 'ask-exec-ask-1', author: 'agent',
      text: 'You edited the block I was writing: keep yours? (Keep mine / Keep yours)\nWhich branch?',
      atMs: NOW + 5, origin: 'ask',
    }]);
  });

  const started = () => row({
    id: 39, method: 'item/started',
    params: JSON.stringify({
      item: { arguments: questions('Heads up?'), status: 'inProgress', tool: USER_ASK_TOOL, type: 'mcpToolCall' },
    }),
  });

  it('draws item/started as a running line that the completed row replaces in place', () => {
    expect(buildTranscript([started()])).toMatchObject([{ author: 'activity', state: 'running', verb: 'Calling' }]);
    const completed = row({ params: askParams({ arguments: questions('Heads up?') }) });
    const entries = buildTranscript([completed, started()]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ id: 'ask-exec-ask-1', author: 'agent', text: 'Heads up?', atMs: NOW + 5 });
  });

  it('draws a refused ask as a failed line, not a bubble', () => {
    const refused = row({
      params: askParams({
        arguments: { questions: [{ title: 'Merge?', choices: ['yes'] }] },
        error: { message: 'tool call error: tool call failed\n\nCaused by:\n    Mcp error: -32602: neige_user_ask: invalid args' },
        result: null, status: 'failed',
      }),
    });
    const entries = buildTranscript([started(), refused]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      author: 'activity', state: 'failed', verb: 'Called',
      detail: 'Mcp error: -32602: neige_user_ask: invalid args',
    });
    expect(entries.some(isAskTurn)).toBe(false);
    const statusOnly = row({ params: askParams({ status: 'failed' }) });
    expect(buildTranscript([statusOnly]).map((entry) => entry.author)).toEqual(['activity']);
  });

  it('leaves every other tool call an activity line', () => {
    const other = row({ params: askParams({ tool: REPORT_READ_TOOLS[0] }) });
    const [entry] = buildTranscript([other]);
    expect(entry?.author).toBe('activity');
    expect(entry).toMatchObject({ verb: 'Read report' });
  });

  it('is not a message without a question', () => {
    const blank = row({ params: askParams({ arguments: questions('   ') }) });
    expect(buildTranscript([blank]).map((entry) => entry.author)).toEqual(['activity']);
    const missing = row({ params: askParams({ arguments: {} }) });
    expect(buildTranscript([missing]).map((entry) => entry.author)).toEqual(['activity']);
  });
});
