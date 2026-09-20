/*
 * The quiet sync: a planner turn woken only by a report edit is background work, and
 * the transcript folds it into one line. Notify speech and failed/interrupted outcomes
 * are lifted out after the fold.
 */

import { REPORT_READ_TOOLS, REPORT_TOOL_PREFIX } from '../keys/mcp-tools.js';
import {
  SYSTEM_PRESENTATION_LABELS,
  type ConversationSystemEntry, type ConversationTurn, type ConversationTurnOutcome,
  type TranscriptEntry,
} from './conversation.js';

/** Who the kernel says edited the report; the wire spellings of `EditAuthor` that wake the planner. */
export type ReportEditAuthor = 'user' | 'assistant' | 'plugin';

export const REPORT_EDIT_AUTHORS: readonly ReportEditAuthor[] = Object.freeze([
  'user', 'assistant', 'plugin',
] as const);

/** `updated`: a report write landed in the turn. `accepted`: no write and the planner said nothing. */
export type QuietSyncOutcome = 'accepted' | 'updated';

/** One folded background turn; `entries[0]` is always the opening system entry. */
export type QuietSyncGroup = Readonly<{
  kind: 'quiet-sync';
  /** The opening system entry's id — stable, so a fold keeps its open state. */
  id: string;
  author: ReportEditAuthor | null;
  atMs: number;
  entries: readonly TranscriptEntry[];
  /** `null` when there is nothing to add to the line: not completed, ended badly, or spoke without writing. */
  outcome: QuietSyncOutcome | null;
}>;

export type TranscriptBlock =
  | Readonly<{ kind: 'entry'; entry: TranscriptEntry }>
  | QuietSyncGroup;

const AUTHOR_IN_TEXT = /\(author = "(user|assistant|plugin)"\)/;

/** The author named on the observation's first line; the older sentence without an author is deliberately `null`. */
export function reportEditAuthor(text: string): ReportEditAuthor | null {
  const firstLine = text.split('\n', 1)[0] ?? '';
  const match = AUTHOR_IN_TEXT.exec(firstLine);
  const author = match?.[1];
  return author === 'user' || author === 'assistant' || author === 'plugin' ? author : null;
}

/** The label is checked as well as `quiet` so the flag can never fold a differently-labelled entry. */
export function isReportEditedEntry(entry: TranscriptEntry): entry is ConversationSystemEntry {
  return entry.author === 'system'
    && entry.label === SYSTEM_PRESENTATION_LABELS.system_report_edited
    && entry.quiet === true;
}

/** Speech the planner meant for the reader: lifted out of a fold, never into one. */
export function isNotifyTurn(entry: TranscriptEntry): entry is ConversationTurn {
  return entry.author === 'agent' && entry.origin === 'notify';
}

/** A turn that did not end well is lifted out of a fold; a `completed` outcome stays inside. */
export function isFailedOutcome(entry: TranscriptEntry): entry is ConversationTurnOutcome {
  return entry.author === 'turn' && entry.status !== 'completed';
}

/** What closes a group: the reader speaking or the kernel delivering the next observation. */
function closesGroup(entry: TranscriptEntry): boolean {
  return entry.author === 'you' || entry.author === 'system';
}

/** Fail-closed on purpose: an unknown report tool is a change, never a look. */
export function isReportWriteTool(tool: string): boolean {
  return tool.startsWith(REPORT_TOOL_PREFIX) && !REPORT_READ_TOOLS.includes(tool);
}

/** A report write that landed; a refused or running one changed nothing. */
export function isReportWriteActivity(entry: TranscriptEntry): boolean {
  return entry.author === 'activity' && entry.state === 'done'
    && entry.tool !== null && isReportWriteTool(entry.tool);
}

function completedOutcome(entry: TranscriptEntry): boolean {
  return entry.author === 'turn' && entry.status === 'completed';
}

/** The line's verdict for one group: what it holds (`grouped`) and what was lifted out of it. */
export function quietSyncOutcome(
  grouped: readonly TranscriptEntry[], lifted: readonly TranscriptEntry[],
): QuietSyncOutcome | null {
  if (!grouped.some(completedOutcome)) return null;
  if (grouped.some(isReportWriteActivity)) return 'updated';
  return lifted.some(isNotifyTurn) ? null : 'accepted';
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
    const lifted: TranscriptEntry[] = [];
    let next = index + 1;
    while (next < entries.length) {
      const candidate = entries[next];
      if (candidate === undefined || closesGroup(candidate)) break;
      if (isNotifyTurn(candidate) || isFailedOutcome(candidate)) lifted.push(candidate);
      else grouped.push(candidate);
      next += 1;
    }
    blocks.push({
      kind: 'quiet-sync',
      id: entry.id,
      author: reportEditAuthor(entry.text),
      atMs: entry.atMs,
      entries: grouped,
      outcome: quietSyncOutcome(grouped, lifted),
    });
    for (const raised of lifted) blocks.push({ kind: 'entry', entry: raised });
    index = next;
  }
  return blocks;
}
