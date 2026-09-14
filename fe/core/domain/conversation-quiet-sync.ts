/*
 * #1667 D3 — the quiet sync: a planner turn that a report edit woke is
 * background work, and the transcript folds it.
 *
 * The kernel wakes the planner when somebody else edits the report
 * (`system_report_edited`). Since #1667 that wake is information — a block
 * diff — and the prompt tells the planner to reconcile silently: read the
 * diff, write only if it must, and say nothing unless there is a conflict, a
 * parse failure or a decision to make. What the transcript held for such a
 * turn was a system line, a few activity lines and, more often than not, a
 * reply saying it had re-read the report. Rendered like any other exchange,
 * that reply was the "system receipt" the owner asked to be rid of.
 *
 * So the transcript is regrouped here, once, on the way to the renderer: a
 * `system_report_edited` entry opens a group that swallows everything after
 * it — activity lines, agent messages, turn outcomes — up to the next user
 * turn or the next system entry. The renderer draws the group as one folded
 * line. Two consecutive edit wakes are two groups, never one: each is a
 * separate sync the reader may want to open on its own.
 *
 * The one thing that is NOT swallowed is speech the planner meant for the
 * reader: a `calm.user.notify` call. `conversation.ts` already turns that
 * tool call into an agent turn marked `origin: 'notify'`; here such a turn is
 * lifted out of the group and placed after it, so the reader sees the fold
 * line and then the bubble. It is placed after rather than at its original
 * position because the alternative splits one sync into two folded lines
 * around one sentence, and "the agent said this during that sync" reads
 * better as line-then-bubble than as fold-bubble-fold.
 *
 * Pure and platform-independent: `TranscriptEntry[]` in, blocks out. The
 * author of the edit is read off the observation's own first line
 * (`author = "user"`), the same spelling the kernel renders for the planner —
 * the front end has no structured author on a system entry, and inventing a
 * second channel for one word was not worth a wire change.
 */

import {
  SYSTEM_PRESENTATION_LABELS,
  type ConversationSystemEntry, type ConversationTurn, type TranscriptEntry,
} from './conversation.js';

/** Who the kernel says edited the report; the wire spellings of `EditAuthor`
 *  that wake the planner (`dispatcher::PLANNER_WAKE_AUTHORS`). */
export type ReportEditAuthor = 'user' | 'assistant' | 'plugin';

export const REPORT_EDIT_AUTHORS: readonly ReportEditAuthor[] = Object.freeze([
  'user', 'assistant', 'plugin',
] as const);

/**
 * One folded background turn: the `system_report_edited` entry that opened
 * it and everything the planner did in response, in transcript order.
 * `entries[0]` is always the system entry.
 */
export type QuietSyncGroup = Readonly<{
  kind: 'quiet-sync';
  /** The opening system entry's id — stable, so a fold keeps its open state. */
  id: string;
  author: ReportEditAuthor | null;
  atMs: number;
  entries: readonly TranscriptEntry[];
}>;

export type TranscriptBlock =
  | Readonly<{ kind: 'entry'; entry: TranscriptEntry }>
  | QuietSyncGroup;

const AUTHOR_IN_TEXT = /\(author = "(user|assistant|plugin)"\)/;

/**
 * The author named on the observation's first line, or `null` when the text
 * names none — the pre-#1252 sentence ("The user edited …") is deliberately
 * `null` rather than `'user'`: it was written before the kernel knew who
 * edited, and mislabelled every plugin edit as a user edit.
 */
export function reportEditAuthor(text: string): ReportEditAuthor | null {
  const firstLine = text.split('\n', 1)[0] ?? '';
  const match = AUTHOR_IN_TEXT.exec(firstLine);
  const author = match?.[1];
  return author === 'user' || author === 'assistant' || author === 'plugin' ? author : null;
}

export function isReportEditedEntry(entry: TranscriptEntry): entry is ConversationSystemEntry {
  return entry.author === 'system' && entry.label === SYSTEM_PRESENTATION_LABELS.system_report_edited;
}

/** Speech the planner meant for the reader: lifted out of a fold, never into one. */
export function isNotifyTurn(entry: TranscriptEntry): entry is ConversationTurn {
  return entry.author === 'agent' && entry.origin === 'notify';
}

/** What closes a group: the reader speaking (a persisted or optimistic user
 *  turn) or the kernel delivering the next observation. */
function closesGroup(entry: TranscriptEntry): boolean {
  return entry.author === 'you' || entry.author === 'system';
}

export function foldQuietSyncs(entries: readonly TranscriptEntry[]): readonly TranscriptBlock[] {
  const blocks: TranscriptBlock[] = [];
  let index = 0;
  while (index < entries.length) {
    const entry = entries[index];
    if (entry === undefined) break;
    if (!isReportEditedEntry(entry)) {
      blocks.push({ kind: 'entry', entry });
      index += 1;
      continue;
    }
    const grouped: TranscriptEntry[] = [entry];
    const lifted: ConversationTurn[] = [];
    let next = index + 1;
    while (next < entries.length) {
      const candidate = entries[next];
      if (candidate === undefined || closesGroup(candidate)) break;
      if (isNotifyTurn(candidate)) lifted.push(candidate);
      else grouped.push(candidate);
      next += 1;
    }
    blocks.push({
      kind: 'quiet-sync',
      id: entry.id,
      author: reportEditAuthor(entry.text),
      atMs: entry.atMs,
      entries: grouped,
    });
    for (const turn of lifted) blocks.push({ kind: 'entry', entry: turn });
    index = next;
  }
  return blocks;
}
