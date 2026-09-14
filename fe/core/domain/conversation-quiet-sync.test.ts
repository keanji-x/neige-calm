import { describe, expect, it } from 'vitest';

import { REPORT_READ_TOOLS, USER_NOTIFY_TOOL } from '../keys/mcp-tools.js';
import {
  REPORT_EDIT_AUTHORS, foldQuietSyncs, isNotifyTurn, reportEditAuthor,
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

function reportEdited(id: string, text = DIFF_TEXT): ConversationSystemEntry {
  return {
    id, author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text, atMs: NOW,
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

function activity(id: string): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Read report', target: null, state: 'done',
    durationMs: null, detail: null, atMs: NOW,
  };
}

function outcome(id: string): ConversationTurnOutcome {
  return { id, author: 'turn', turnId: 'turn-1', status: 'completed', atMs: NOW };
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

  it('mints the bubble from item/started too, and the completed row replaces it in place', () => {
    const started = row({
      id: 39, method: 'item/started',
      params: JSON.stringify({
        item: { arguments: { text: 'Heads up' }, status: 'inProgress', tool: USER_NOTIFY_TOOL, type: 'mcpToolCall' },
      }),
    });
    const completed = row({ params: notifyParams({ arguments: { text: 'Heads up' } }) });
    const entries = buildTranscript([completed, started]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ id: 'notify-exec-notify-1', author: 'agent', text: 'Heads up', atMs: NOW + 5 });
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
