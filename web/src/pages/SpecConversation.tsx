// SpecConversation (#654) — document-mode spec conversation.
//
// The report column toggles between the report document (children) and a
// conversation document rendered from the spec card's chat history. Both
// share the same column width and typographic rhythm; a single minimal
// input line lives at the bottom of the column in both modes, so the
// draft (and focus) survives the mode switch. Sending from report mode
// auto-switches to conversation mode.
import { useEffect, useRef, type ReactNode } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useState } from '../shared/state';
import { ConfirmDialog } from '../ui/ConfirmDialog/ConfirmDialog';
import {
  humanizeToken,
  useSpecCurrentRun,
} from './useSpecCurrentRun';
import {
  useSpecChatHistory,
  type VisibleChatEntry,
} from './useSpecChatHistory';

export type ReportView = 'report' | 'conversation';

export interface SpecConversationProps {
  /** Spec card id; null/undefined when wave has no spec card. */
  specCardId: string | null;
  /** Which document the column shows. */
  view: ReportView;
  onViewChange(view: ReportView): void;
  /** The report document, shown when `view === 'report'`. */
  children: ReactNode;
}

const MARKDOWN_PLUGINS = [remarkGfm];

function entryTitle(atMs: number): string {
  return new Date(atMs).toLocaleString();
}

function entryClock(atMs: number): string {
  const d = new Date(atMs);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `${hh}:${mm}`;
}

function scrollToBottom(node: HTMLDivElement | null): void {
  if (!node) return;
  node.scrollTop = node.scrollHeight;
}

function ConvoTypingIndicator() {
  return (
    <div
      className="report-convo-typing"
      role="status"
      aria-label="Spec Agent is working"
    >
      <span className="report-convo-typing-dot" aria-hidden="true" />
      <span className="report-convo-typing-dot" aria-hidden="true" />
      <span className="report-convo-typing-dot" aria-hidden="true" />
    </div>
  );
}

function EntryMeta({
  author,
  atMs,
}: {
  author: string;
  atMs: number;
}) {
  return (
    <div className="report-convo-meta">
      <span className="report-convo-author">{author}</span>
      <time className="report-convo-time" title={entryTitle(atMs)}>
        {entryClock(atMs)}
      </time>
    </div>
  );
}

function ConvoEntry({
  entry,
  expanded,
  onToggleExpanded,
}: {
  entry: VisibleChatEntry;
  expanded: boolean;
  onToggleExpanded(id: number): void;
}) {
  if (entry.kind === 'system') {
    return (
      <div className="report-convo-system" title={entryTitle(entry.atMs)}>
        &middot; {entry.label ?? entry.text} &middot;
      </div>
    );
  }

  if (entry.kind === 'agent') {
    return (
      <section className="report-convo-entry report-convo-entry--agent">
        <EntryMeta author="Spec Agent" atMs={entry.atMs} />
        <div className="report-prose report-convo-body">
          <ReactMarkdown remarkPlugins={MARKDOWN_PLUGINS}>
            {entry.text}
          </ReactMarkdown>
        </div>
      </section>
    );
  }

  const clamped = entry.clamp === true && !expanded;

  return (
    <section
      className={
        'report-convo-entry report-convo-entry--user' +
        (entry.queued ? ' report-convo-entry--queued' : '')
      }
    >
      <EntryMeta
        author={entry.queued ? 'You · queued' : 'You'}
        atMs={entry.atMs}
      />
      <p
        className={
          'report-convo-user-text' +
          (clamped ? ' report-convo-user-text--clamped' : '')
        }
      >
        {entry.text}
      </p>
      {entry.clamp === true && (
        <button
          type="button"
          className="report-convo-expand"
          aria-expanded={expanded}
          onClick={() => onToggleExpanded(entry.id)}
        >
          {expanded ? 'Show less' : 'Show more'}
        </button>
      )}
    </section>
  );
}

export function SpecConversation({
  specCardId,
  view,
  onViewChange,
  children,
}: SpecConversationProps) {
  const run = useSpecCurrentRun(specCardId ?? undefined);
  const chatHistory = useSpecChatHistory(specCardId ?? undefined);
  const [draft, setDraft] = useState('');
  const [resetOpen, setResetOpen] = useState(false);
  const [resetAttempted, setResetAttempted] = useState(false);
  const [expandedEntries, setExpandedEntries] = useState<Set<number>>(
    () => new Set(),
  );
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickToBottomRef = useRef(true);
  const latestSpecCardIdRef = useRef<string | null>(specCardId);
  latestSpecCardIdRef.current = specCardId;

  // Guard: the conversation document needs a spec card.
  const inConversation = view === 'conversation' && specCardId != null;

  useEffect(() => {
    if (!inConversation) return;
    stickToBottomRef.current = true;
    const id = window.setTimeout(() => {
      scrollToBottom(scrollRef.current);
      textareaRef.current?.focus();
    }, 30);
    return () => window.clearTimeout(id);
  }, [inConversation]);

  useEffect(() => {
    setExpandedEntries(new Set());
  }, [specCardId]);

  useEffect(() => {
    if (!inConversation || !stickToBottomRef.current) return;
    const id = window.setTimeout(() => {
      scrollToBottom(scrollRef.current);
    }, 0);
    return () => window.clearTimeout(id);
  }, [chatHistory.entries.length, inConversation, run.fsm]);

  // Auto-grow the single-line textarea with the draft.
  useEffect(() => {
    const node = textareaRef.current;
    if (!node) return;
    node.style.height = 'auto';
    node.style.height = `${Math.min(node.scrollHeight, 160)}px`;
  }, [draft]);

  const onSubmit = async () => {
    const text = draft.trim();
    if (!text || run.submitPending || run.resetPending) return;
    const cardIdAtSubmit = specCardId;
    if (view !== 'conversation') {
      // Sending from report mode lands the user in the conversation.
      onViewChange('conversation');
    }
    try {
      await run.submit(text);
      // If the component has been reused for another card, the draft now
      // belongs to that card.
      if (cardIdAtSubmit !== latestSpecCardIdRef.current) return;
      chatHistory.addEcho(text);
      setDraft('');
    } catch {
      // submitError is captured by useSpecCurrentRun and rendered below.
    }
  };

  const onConfirmReset = async () => {
    const cardIdAtReset = specCardId;
    setResetAttempted(true);
    try {
      await run.reset();
      if (cardIdAtReset !== latestSpecCardIdRef.current) return;
      setResetOpen(false);
      setResetAttempted(false);
    } catch {
      // resetError is captured by useSpecCurrentRun and rendered in-dialog.
    }
  };

  const onScroll = () => {
    const node = scrollRef.current;
    if (!node) return;
    const distanceFromBottom =
      node.scrollHeight - node.scrollTop - node.clientHeight;
    stickToBottomRef.current = distanceFromBottom <= 40;
  };

  const toggleExpandedEntry = (id: number) => {
    setExpandedEntries((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const isWorking = run.fsm === 'Working';
  const historyIsEmpty =
    chatHistory.entries.length === 0 && !isWorking && !chatHistory.hasEarlier;

  return (
    <>
      <header className="report-convo-head">
        <div className="report-convo-head-inner">
          <div
            role="tablist"
            aria-label="Document view"
            className="report-convo-tabs"
          >
            <button
              type="button"
              role="tab"
              aria-selected={!inConversation}
              className="report-convo-tab"
              onClick={() => onViewChange('report')}
            >
              Report
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={inConversation}
              className="report-convo-tab"
              disabled={specCardId == null}
              title={specCardId == null ? 'Spec agent unavailable' : undefined}
              onClick={() => onViewChange('conversation')}
            >
              Conversation
            </button>
          </div>
          {inConversation && (
            <span className="report-convo-status" aria-label="Spec agent status">
              <span
                className="report-convo-state"
                data-fsm={run.fsm}
                title={run.rawState}
              >
                {humanizeToken(run.rawState)}
              </span>
              {run.phase && (
                <span className="report-convo-phase">
                  {humanizeToken(run.phase)}
                </span>
              )}
              <button
                type="button"
                className="report-convo-reset"
                aria-label="Reset spec session"
                disabled={run.resetPending}
                onClick={() => {
                  setResetAttempted(false);
                  setResetOpen(true);
                }}
              >
                Reset
              </button>
            </span>
          )}
        </div>
      </header>

      <div
        ref={scrollRef}
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable document column must be keyboard-focusable.
        tabIndex={0}
        role="region"
        aria-label={inConversation ? 'Conversation' : 'Report document'}
        className="report-convo-scroll"
        onScroll={onScroll}
      >
        {!inConversation && children}
        {inConversation && (
          <div className="report-convo-doc">
            {chatHistory.hasEarlier && (
              <button
                type="button"
                className="report-convo-load-earlier"
                disabled={chatHistory.loadEarlierPending}
                onClick={() => {
                  stickToBottomRef.current = false;
                  void chatHistory.loadEarlier();
                }}
              >
                {chatHistory.loadEarlierPending ? 'Loading...' : 'Load earlier'}
              </button>
            )}

            {historyIsEmpty && (
              <p className="report-convo-empty">
                No messages yet &mdash; ask the Spec Agent below.
              </p>
            )}

            {chatHistory.entries.map((entry) => (
              <ConvoEntry
                key={`${entry.queued ? 'queued' : 'item'}:${entry.id}`}
                entry={entry}
                expanded={expandedEntries.has(entry.id)}
                onToggleExpanded={toggleExpandedEntry}
              />
            ))}

            {isWorking && <ConvoTypingIndicator />}
          </div>
        )}
      </div>

      {specCardId != null && (
        <footer className="report-convo-inputbar">
          <div className="report-convo-inputbar-inner">
            {run.submitError && (
              <p className="report-convo-error" role="alert">
                {run.submitError}
              </p>
            )}
            <div
              className={
                'report-convo-inputline' +
                (run.submitPending ? ' report-convo-inputline--pending' : '')
              }
            >
              <textarea
                ref={textareaRef}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  const isComposing =
                    e.nativeEvent.isComposing === true || e.keyCode === 229;
                  if (isComposing) return;

                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    void onSubmit();
                  }
                }}
                placeholder="Ask the Spec Agent…"
                enterKeyHint="send"
                rows={1}
                disabled={run.submitPending || run.resetPending}
                aria-label="Ask the Spec Agent"
                aria-describedby="report-convo-hint"
              />
              <span id="report-convo-hint" className="sr-only">
                Press Enter to send; Shift+Enter inserts a newline.
              </span>
              {draft.trim() !== '' && (
                <button
                  type="button"
                  className="report-convo-send"
                  aria-label="Send"
                  disabled={run.submitPending || run.resetPending}
                  onClick={() => {
                    void onSubmit();
                  }}
                >
                  &#8629;
                </button>
              )}
            </div>
          </div>
        </footer>
      )}

      <ConfirmDialog
        open={resetOpen}
        title="Reset spec session?"
        description={
          <>
            <p>
              This kills the current spec daemon and starts a new conversation.
              The wave report is preserved, but the spec conversation transcript
              will be discarded.
            </p>
            {resetAttempted && run.resetError && (
              <p className="report-convo-error" role="alert">
                {run.resetError}
              </p>
            )}
          </>
        }
        confirmLabel="Reset session"
        cancelLabel="Cancel"
        destructive
        confirmDisabled={run.resetPending}
        onConfirm={() => {
          void onConfirmReset();
        }}
        onCancel={() => {
          setResetOpen(false);
          setResetAttempted(false);
        }}
      />
    </>
  );
}
