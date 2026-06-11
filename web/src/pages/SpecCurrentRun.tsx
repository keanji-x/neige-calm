import { useEffect, useRef } from 'react';
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

export interface SpecCurrentRunProps {
  /** Spec card id; null/undefined when wave has no spec card. */
  specCardId: string | null;
}

const MARKDOWN_PLUGINS = [remarkGfm];

function entryTitle(atMs: number): string {
  return new Date(atMs).toLocaleString();
}

function scrollToHistoryBottom(node: HTMLDivElement | null): void {
  if (!node) return;
  node.scrollTop = node.scrollHeight;
}

function SpecChatTypingIndicator() {
  return (
    <div
      className="report-chat-entry report-chat-entry--agent report-chat-typing"
      role="status"
      aria-label="Spec Agent is working"
    >
      <span className="report-chat-typing-dot" aria-hidden="true" />
      <span className="report-chat-typing-dot" aria-hidden="true" />
      <span className="report-chat-typing-dot" aria-hidden="true" />
    </div>
  );
}

function SpecChatEntry({
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
      <div className="report-chat-system" title={entryTitle(entry.atMs)}>
        &middot; {entry.label ?? entry.text} &middot;
      </div>
    );
  }

  if (entry.kind === 'agent') {
    return (
      <div className="report-chat-entry report-chat-entry--agent">
        <article
          className="report-chat-agent report-chat-md"
          title={entryTitle(entry.atMs)}
        >
          <ReactMarkdown remarkPlugins={MARKDOWN_PLUGINS}>
            {entry.text}
          </ReactMarkdown>
        </article>
      </div>
    );
  }

  const clamped = entry.clamp === true && !expanded;

  return (
    <div
      className={
        'report-chat-entry report-chat-entry--user' +
        (entry.queued ? ' report-chat-entry--queued' : '')
      }
    >
      <div
        className="report-chat-bubble report-chat-bubble--user"
        title={entryTitle(entry.atMs)}
      >
        <p
          className={
            'report-chat-user-text' +
            (clamped ? ' report-chat-user-text--clamped' : '')
          }
        >
          {entry.text}
        </p>
        {entry.queued && (
          <span className="report-chat-queued-chip">Queued</span>
        )}
      </div>
      {entry.clamp === true && (
        <button
          type="button"
          className="report-chat-expand"
          aria-expanded={expanded}
          onClick={() => onToggleExpanded(entry.id)}
        >
          {expanded ? 'Show less' : 'Show more'}
        </button>
      )}
    </div>
  );
}

export function SpecCurrentRun({ specCardId }: SpecCurrentRunProps) {
  const run = useSpecCurrentRun(specCardId ?? undefined);
  const chatHistory = useSpecChatHistory(specCardId ?? undefined);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState('');
  const [resetOpen, setResetOpen] = useState(false);
  const [resetAttempted, setResetAttempted] = useState(false);
  const [expandedEntries, setExpandedEntries] = useState<Set<number>>(
    () => new Set(),
  );
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const historyRef = useRef<HTMLDivElement>(null);
  const stickToBottomRef = useRef(true);
  const latestSpecCardIdRef = useRef<string | null>(specCardId);
  latestSpecCardIdRef.current = specCardId;

  useEffect(() => {
    if (!open) return;
    stickToBottomRef.current = true;
    const id = window.setTimeout(() => {
      scrollToHistoryBottom(historyRef.current);
      textareaRef.current?.focus();
    }, 30);
    return () => window.clearTimeout(id);
  }, [open]);

  useEffect(() => {
    setExpandedEntries(new Set());
  }, [specCardId]);

  useEffect(() => {
    if (!open || !stickToBottomRef.current) return;
    const id = window.setTimeout(() => {
      scrollToHistoryBottom(historyRef.current);
    }, 0);
    return () => window.clearTimeout(id);
  }, [chatHistory.entries.length, open, run.fsm]);

  if (specCardId == null) {
    return (
      <div
        className="report-chat report-chat--disabled"
        aria-label="Ask the spec agent"
      >
        <span className="report-chat-pill report-chat-pill--disabled">
          <span className="report-chat-avatar" aria-hidden="true">
            S
          </span>
          <span className="report-chat-label">Spec agent unavailable</span>
        </span>
      </div>
    );
  }

  const onSubmit = async () => {
    const text = draft.trim();
    if (!text || run.submitPending || run.resetPending) return;
    const cardIdAtSubmit = specCardId;
    try {
      await run.submit(text);
      // If the component has been reused for another card, the draft/open
      // state now belongs to that card.
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

  const onHistoryScroll = () => {
    const node = historyRef.current;
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
    <div
      className={'report-chat' + (open ? ' report-chat--open' : '')}
      aria-label="Ask the spec agent"
    >
      {!open && (
        <button
          type="button"
          className="report-chat-pill"
          onClick={() => setOpen(true)}
        >
          <span className="report-chat-avatar" aria-hidden="true">
            S
          </span>
          <span className="report-chat-label">Ask the Spec Agent</span>
        </button>
      )}

      {open && (
        <section
          className="report-chat-box"
          aria-label="Ask the Spec Agent"
        >
          <header className="report-chat-head">
            <div className="report-chat-who">
              <span className="report-chat-avatar" aria-hidden="true">
                S
              </span>
              <span className="report-chat-name">Spec Agent</span>
              <span className="report-chat-status">
                <span
                  className="report-chat-state"
                  data-fsm={run.fsm}
                  title={run.rawState}
                >
                  {humanizeToken(run.rawState)}
                </span>
                {run.phase && (
                  <span className="report-chat-phase">
                    {humanizeToken(run.phase)}
                  </span>
                )}
                <button
                  type="button"
                  className="report-chat-reset-pill"
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
            </div>
            <button
              type="button"
              className="report-chat-close"
              aria-label="Close"
              onClick={() => setOpen(false)}
            >
              &times;
            </button>
          </header>

          <div
            ref={historyRef}
            role="region"
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable transcript must be keyboard-focusable.
            tabIndex={0}
            aria-label="Spec chat history"
            className={
              'report-chat-history' +
              (historyIsEmpty ? ' report-chat-history--empty' : '')
            }
            onScroll={onHistoryScroll}
          >
            {chatHistory.hasEarlier && (
              <button
                type="button"
                className="report-chat-load-earlier"
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
              <p className="report-chat-empty">
                No messages yet &mdash; ask a follow-up about this report.
              </p>
            )}

            {chatHistory.entries.map((entry) => (
              <SpecChatEntry
                key={`${entry.queued ? 'queued' : 'item'}:${entry.id}`}
                entry={entry}
                expanded={expandedEntries.has(entry.id)}
                onToggleExpanded={toggleExpandedEntry}
              />
            ))}

            {isWorking && <SpecChatTypingIndicator />}
          </div>

          {run.latestTool.toolLabel != null && (
            <div className="report-chat-tool" aria-label="Latest tool">
              <span className="report-chat-tool-label">
                {run.latestTool.toolLabel}
              </span>
              {run.latestTool.toolStatus && (
                <span className="report-chat-tool-status">
                  {humanizeToken(run.latestTool.toolStatus)}
                </span>
              )}
            </div>
          )}

          <div
            className={
              'report-chat-input' +
              (run.submitPending ? ' report-chat-input--pending' : '')
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
              placeholder="Ask a follow-up about this report..."
              enterKeyHint="send"
              rows={1}
              disabled={run.submitPending || run.resetPending}
              aria-label="Follow-up"
              aria-describedby="report-chat-hint"
            />
            <span id="report-chat-hint" className="sr-only">
              Press Enter to send; Shift+Enter inserts a newline.
            </span>
          </div>

          {run.submitError && (
            <p className="report-chat-error" role="alert">
              {run.submitError}
            </p>
          )}
        </section>
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
              <p className="report-chat-error" role="alert">
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
    </div>
  );
}
