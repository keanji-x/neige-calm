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
 * `system_report_edited` entry that was the WHOLE of its batch
 * (`ConversationSystemEntry.quiet`, set where `conversation.ts` converts the
 * persisted segments) opens a group that swallows everything after it —
 * activity lines, agent messages, completed turn outcomes — up to the next
 * user turn or the next system entry. The renderer draws the group as one
 * folded line. Two consecutive edit wakes are two groups, never one: each is
 * a separate sync the reader may want to open on its own.
 *
 * The batch rule is the kernel's, mirrored (round-4 M1): a turn is a quiet
 * sync when its queue held nothing but report edits
 * (`run_loop.rs` `queue_is_only_report_edits`). A report edit that rode
 * along with a user message or a task event opened an ordinary turn in which
 * the planner answers the user or the event; its `system_report_edited`
 * segment is not marked `quiet`, so it is drawn as a plain system line and
 * the reply after it stays visible. Folding on the label alone hid exactly
 * that reply.
 *
 * Two things are NOT swallowed. Speech the planner meant for the reader: a
 * `calm.user.notify` call, which `conversation.ts` already turns into an
 * agent turn marked `origin: 'notify'`. And the turn ending badly: a
 * `failed` or `interrupted` outcome (round-4 N2) — a sync that stopped on a
 * model error would otherwise leave no visible sign at all, a closed fold
 * being what a finished sync looks like too. Both are lifted out of the
 * group and placed after it, so the reader sees the fold line and then the
 * bubble or the red line. Placed after rather than at their original
 * position because the alternative splits one sync into two folded lines
 * around one sentence, and "the agent said this during that sync" reads
 * better as line-then-bubble than as fold-bubble-fold.
 *
 * Pure and platform-independent: `TranscriptEntry[]` in, blocks out. The
 * author of the edit is read off the observation's own first line
 * (`author = "user"`), the same spelling the kernel renders for the planner —
 * the front end has no structured author on a system entry, and inventing a
 * second channel for one word was not worth a wire change.
 *
 * #1678 A4 — a closed fold looks the same whether the sync took the edit in
 * or missed it, so a finished group also carries what the turn did to the
 * report (`outcome`), read off the entries it holds: the completed turn
 * outcome says it finished, and a `calm.report.*` write that landed says the
 * report changed. The write test is fail-closed on the tool name
 * (`isReportWriteTool`): anything under the report prefix that is not a
 * known read is a change, so a tool this code has never heard of can never
 * read as "no action".
 */

import { REPORT_READ_TOOLS, REPORT_TOOL_PREFIX } from '../keys/mcp-tools.js';
import {
  SYSTEM_PRESENTATION_LABELS,
  type ConversationSystemEntry, type ConversationTurn, type ConversationTurnOutcome,
  type TranscriptEntry,
} from './conversation.js';

/** Who the kernel says edited the report; the wire spellings of `EditAuthor`
 *  that wake the planner (`dispatcher::PLANNER_WAKE_AUTHORS`). */
export type ReportEditAuthor = 'user' | 'assistant' | 'plugin';

export const REPORT_EDIT_AUTHORS: readonly ReportEditAuthor[] = Object.freeze([
  'user', 'assistant', 'plugin',
] as const);

/**
 * What a finished sync did to the report. `updated`: a report write landed
 * in the turn. `accepted`: the turn completed with no report write and the
 * planner said nothing — the edit was taken as it is.
 */
export type QuietSyncOutcome = 'accepted' | 'updated';

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
  /** `null` when there is nothing to add to the line: the turn has not
   *  completed (the fold carries the live mark), it ended badly (the lifted
   *  red line says so), or the planner spoke without writing (the lifted
   *  notify bubble is the outcome). */
  outcome: QuietSyncOutcome | null;
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

/** The report-edit wake that opens a fold: a `system_report_edited` entry
 *  whose batch was nothing else (`quiet`). The label is checked as well so
 *  the flag can never fold a differently-labelled entry, whatever minted it. */
export function isReportEditedEntry(entry: TranscriptEntry): entry is ConversationSystemEntry {
  return entry.author === 'system'
    && entry.label === SYSTEM_PRESENTATION_LABELS.system_report_edited
    && entry.quiet === true;
}

/** Speech the planner meant for the reader: lifted out of a fold, never into one. */
export function isNotifyTurn(entry: TranscriptEntry): entry is ConversationTurn {
  return entry.author === 'agent' && entry.origin === 'notify';
}

/** A turn that did not end well: the reader must see it, so it is lifted out
 *  of a fold like a notify. A `completed` outcome renders as nothing and stays
 *  inside. */
export function isFailedOutcome(entry: TranscriptEntry): entry is ConversationTurnOutcome {
  return entry.author === 'turn' && entry.status !== 'completed';
}

/** What closes a group: the reader speaking (a persisted or optimistic user
 *  turn) or the kernel delivering the next observation. */
function closesGroup(entry: TranscriptEntry): boolean {
  return entry.author === 'you' || entry.author === 'system';
}

/** A `calm.report.*` call that is not one of the reads. Fail-closed on
 *  purpose: an unknown report tool is a change, never a look. */
export function isReportWriteTool(tool: string): boolean {
  return tool.startsWith(REPORT_TOOL_PREFIX) && !REPORT_READ_TOOLS.includes(tool);
}

/** A report write that landed. A refused one (a 409, drawn as a failed line
 *  inside the fold) changed nothing, and a running one has not yet. */
export function isReportWriteActivity(entry: TranscriptEntry): boolean {
  return entry.author === 'activity' && entry.state === 'done'
    && entry.tool !== null && isReportWriteTool(entry.tool);
}

function completedOutcome(entry: TranscriptEntry): boolean {
  return entry.author === 'turn' && entry.status === 'completed';
}

/** The line's verdict for one group: what it holds (`grouped`) and what was
 *  lifted out of it (`lifted`: notify bubbles, bad outcomes). */
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
