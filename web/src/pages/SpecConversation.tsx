// SpecConversation (#654/#975) — the report page's conversation drawer.
import { useEffect, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useState } from '../shared/state';
import { ConfirmDialog } from '../ui/ConfirmDialog/ConfirmDialog';
import {
  humanizeToken,
  type SpecRunSnapshot,
} from './useSpecCurrentRun';
import {
  type SpecChatHistorySnapshot,
  type VisibleChatEntry,
} from './useSpecChatHistory';

export interface SpecConversationProps {
  /** Spec card id; null/undefined when track has no spec card. */
  specCardId: string | null;
  /** Whether the persistently-mounted drawer is currently open. */
  drawerOpen: boolean;
  run: SpecRunSnapshot;
  chatHistory: SpecChatHistorySnapshot;
  /** Entry selected from the report activity panel. */
  targetEntryId?: number | null;
  onTargetConsumed?(): void;
  /** Close control supplied by the report shell. */
  onClose?: () => void;
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

function scrollToBottom(column: HTMLDivElement | null): void {
  if (!column) return;
  column.scrollTop = column.scrollHeight;
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
      <span className="report-convo-typing-hint">Esc to stop</span>
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

function durationText(durationMs: number | null): string | null {
  if (durationMs == null) return null;
  return `${durationMs}ms`;
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
        <div className="report-prose report-convo-body calm-prose">
          <ReactMarkdown remarkPlugins={MARKDOWN_PLUGINS}>
            {entry.text}
          </ReactMarkdown>
        </div>
      </section>
    );
  }

  if (entry.kind === 'run') {
    const exitFailed = entry.exitCode != null && entry.exitCode !== 0;
    const statusWarn = entry.status !== '' && entry.status !== 'completed';
    const duration = durationText(entry.durationMs);
    return (
      <section
        className={
          'report-convo-entry report-convo-entry--run' +
          (statusWarn ? ' report-convo-entry--warn' : '')
        }
      >
        <EntryMeta author="Command" atMs={entry.atMs} />
        <div className="report-convo-artifact-head">
          <code className="report-convo-command">
            {entry.command || '(command)'}
          </code>
          {statusWarn && (
            <span className="report-convo-chip report-convo-chip--warn">
              {entry.status}
            </span>
          )}
          <span
            className={
              'report-convo-chip' +
              (exitFailed ? ' report-convo-chip--fail' : '')
            }
          >
            exit {entry.exitCode ?? 'n/a'}
          </span>
          {duration != null && (
            <span className="report-convo-chip">{duration}</span>
          )}
        </div>
        {entry.output && (
          <details className="report-convo-details">
            <summary>Output</summary>
            <pre>{entry.output}</pre>
          </details>
        )}
      </section>
    );
  }

  if (entry.kind === 'tool') {
    const duration = durationText(entry.durationMs);
    const author =
      [entry.server, entry.tool].filter(Boolean).join(' · ') || 'Tool';
    const statusWarn = entry.status !== '' && entry.status !== 'completed';
    const warn = entry.isError || statusWarn;
    return (
      <section
        className={
          'report-convo-entry report-convo-entry--tool' +
          (warn ? ' report-convo-entry--warn' : '')
        }
      >
        <EntryMeta author={author} atMs={entry.atMs} />
        <div className="report-convo-artifact-head">
          {entry.isError && (
            <span className="report-convo-chip report-convo-chip--warn">
              error
            </span>
          )}
          {!entry.isError && statusWarn && (
            <span className="report-convo-chip report-convo-chip--warn">
              {entry.status}
            </span>
          )}
          {duration != null && (
            <span className="report-convo-chip">{duration}</span>
          )}
        </div>
        {entry.args && (
          <details className="report-convo-details">
            <summary>Arguments</summary>
            <pre>{entry.args}</pre>
          </details>
        )}
        {entry.result && (
          <details className="report-convo-details">
            <summary>{entry.isError ? 'Error' : 'Result'}</summary>
            <pre>{entry.result}</pre>
          </details>
        )}
      </section>
    );
  }

  if (entry.kind === 'reasoning') {
    const summary = entry.summary.trim();
    const detail = entry.detail.trim();
    return (
      <section className="report-convo-entry report-convo-entry--reasoning">
        <EntryMeta author="Reasoning" atMs={entry.atMs} />
        {summary && (
          <p className="report-convo-reasoning-summary">{entry.summary}</p>
        )}
        {detail && (
          <details className="report-convo-details">
            <summary>Detail</summary>
            <div className="report-convo-reasoning-detail">
              {entry.detail}
            </div>
          </details>
        )}
      </section>
    );
  }

  if (entry.kind === 'edit') {
    const statusWarn = entry.status !== '' && entry.status !== 'completed';
    return (
      <section
        className={
          'report-convo-entry report-convo-entry--edit' +
          (statusWarn ? ' report-convo-entry--warn' : '')
        }
      >
        <EntryMeta author="File changes" atMs={entry.atMs} />
        {statusWarn && (
          <div className="report-convo-artifact-head">
            <span className="report-convo-chip report-convo-chip--warn">
              {entry.status}
            </span>
          </div>
        )}
        {entry.changes.length === 0 ? (
          <p className="report-convo-muted">(file changes)</p>
        ) : (
          <div className="report-convo-change-list">
            {entry.changes.map((change, index) => (
              <div
                className="report-convo-change"
                key={`${change.path}:${index}`}
              >
                <div className="report-convo-change-head">
                  <code>{change.path || '(path)'}</code>
                  <span className="report-convo-chip">
                    {change.verb || 'change'}
                  </span>
                </div>
                {change.diff && (
                  <details className="report-convo-details">
                    <summary>Diff</summary>
                    <pre>{change.diff}</pre>
                  </details>
                )}
              </div>
            ))}
          </div>
        )}
      </section>
    );
  }

  if (entry.kind === 'compact') {
    return (
      <div
        className="report-convo-entry report-convo-system report-convo-entry--compact"
        title={entryTitle(entry.atMs)}
      >
        &middot; context compacted &middot;
      </div>
    );
  }

  if (entry.kind === 'unknown') {
    return (
      <div
        className="report-convo-entry report-convo-system report-convo-entry--unknown"
        title={entryTitle(entry.atMs)}
      >
        &middot; {entry.itemType} &middot;
      </div>
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
  drawerOpen,
  run,
  chatHistory,
  targetEntryId,
  onTargetConsumed,
  onClose,
}: SpecConversationProps) {
  const [draft, setDraft] = useState('');
  const [resetOpen, setResetOpen] = useState(false);
  const [resetAttempted, setResetAttempted] = useState(false);
  const [expandedEntries, setExpandedEntries] = useState<Set<number>>(
    () => new Set(),
  );
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickToBottomRef = useRef(true);
  const latestSpecCardIdRef = useRef<string | null>(specCardId);
  latestSpecCardIdRef.current = specCardId;

  const onAnyScroll = () => {
    if (!drawerOpen) return;
    const target = scrollRef.current;
    if (!target) return;
    const distanceFromBottom =
      target.scrollHeight - target.scrollTop - target.clientHeight;
    stickToBottomRef.current = distanceFromBottom <= 40;
  };

  useEffect(() => {
    if (!drawerOpen || specCardId == null) return;
    stickToBottomRef.current = true;
    const id = window.setTimeout(() => {
      scrollToBottom(scrollRef.current);
      // Also moves focus off an activity-row button once its panel becomes
      // aria-hidden; activity-panel selection relies on this handoff.
      textareaRef.current?.focus();
    }, 30);
    return () => window.clearTimeout(id);
  }, [drawerOpen, specCardId]);

  useEffect(() => {
    if (!drawerOpen || targetEntryId == null) return;
    stickToBottomRef.current = false;
    const id = window.setTimeout(() => {
      const target = scrollRef.current?.querySelector<HTMLElement>(
        `[data-chat-entry-id="${targetEntryId}"]`,
      );
      if (target) {
        target.scrollIntoView({ block: 'center' });
      } else {
        stickToBottomRef.current = true;
        scrollToBottom(scrollRef.current);
      }
      onTargetConsumed?.();
    }, 40);
    return () => window.clearTimeout(id);
  }, [drawerOpen, onTargetConsumed, targetEntryId]);

  useEffect(() => {
    setExpandedEntries(new Set());
  }, [specCardId]);

  useEffect(() => {
    if (!drawerOpen || !stickToBottomRef.current) return;
    const id = window.setTimeout(() => {
      scrollToBottom(scrollRef.current);
    }, 0);
    return () => window.clearTimeout(id);
  }, [chatHistory.entries.length, drawerOpen, run.fsm]);

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
    try {
      await run.submit(text);
      // If the component has been reused for another card, the draft now
      // belongs to that card.
      if (cardIdAtSubmit !== latestSpecCardIdRef.current) return;
      chatHistory.addEcho(text);
      setDraft('');
      // Follow your own send (#654): even if the reader had scrolled up,
      // a successful send should land them on their new message (and the
      // reply that follows). Re-stick and scroll the drawer's own root.
      stickToBottomRef.current = true;
      window.setTimeout(() => {
        scrollToBottom(scrollRef.current);
      }, 0);
    } catch {
      // submitError is captured by useSpecCurrentRun and rendered below.
    }
  };

  const onStop = async () => {
    if (run.stopPending) return;
    const cardIdAtStop = specCardId;
    try {
      const stopped = await run.stop();
      if (cardIdAtStop !== latestSpecCardIdRef.current) return;
      // Interrupted turns never emit `item/completed`, so without this
      // FE-local row the stop would be visually silent (#668). Skipped on
      // the graceful idle no-op — nothing was actually stopped.
      if (stopped) chatHistory.addSystemNote('Turn stopped');
    } catch {
      // stopError is captured by useSpecCurrentRun and rendered below.
    }
  };
  const onStopRef = useRef(onStop);
  onStopRef.current = onStop;

  // #668 fix — gate on the harness phase, not `fsm === 'Working'`: no
  // status overlay is ever published for spec cards, so the overlay-derived
  // fsm never reaches 'Working' in production and every stop affordance
  // stayed dead. `working` comes straight from the phase wire signal.
  const isWorking = run.working;
  // While an interrupt is in flight the stop affordances stay visible but
  // inert — pressing Stop twice can't dispatch a second interrupt.
  const stopVisible = run.working || run.stopping;
  const stopDisabled = run.stopPending || run.stopping;

  // Esc stops the running turn (#668). A document-level listener covers
  // focus anywhere inside the drawer. Esc from the report document, rail,
  // menus, or document.body is left alone, as is an Esc a closer listener
  // already consumed (defaultPrevented). Gated on an open drawer + a
  // live turn (`run.working`; an in-flight interrupt leaves Esc inert),
  // skipped while the reset ConfirmDialog is open (its own Esc-to-cancel
  // wins) and during IME composition; preventDefault only when handled.
  useEffect(() => {
    if (!drawerOpen || !isWorking || resetOpen) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (e.defaultPrevented) return;
      if (e.isComposing || e.keyCode === 229) return;
      const target = e.target;
      const inConversationRegion =
        target instanceof Node && drawerRef.current?.contains(target) === true;
      if (!inConversationRegion) return;
      e.preventDefault();
      void onStopRef.current();
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [drawerOpen, isWorking, resetOpen]);

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

  const historyIsEmpty =
    chatHistory.entries.length === 0 && !isWorking && !chatHistory.hasEarlier;

  return (
    <div ref={drawerRef} className="report-convo">
      <header className="report-convo-head">
        <div className="report-convo-head-inner">
          <h2>Conversation</h2>
          {drawerOpen && specCardId != null && (
            <span className="report-convo-status" aria-label="Spec agent status">
              {/* #668 fix — the chip reflects the harness phase when one is
                  known; the overlay-derived rawState (which never publishes
                  for spec cards) is only the fallback. */}
              <span
                className="report-convo-state"
                data-fsm={run.fsm}
                title={run.phase ?? run.rawState}
              >
                {humanizeToken(run.phase ?? run.rawState)}
              </span>
              {stopVisible && (
                <button
                  type="button"
                  className="report-convo-stop"
                  aria-label="Stop spec turn"
                  disabled={stopDisabled}
                  onClick={() => {
                    void onStop();
                  }}
                >
                  Stop
                </button>
              )}
              <button
                type="button"
                className="report-convo-reset"
                aria-label="Reset spec session"
                data-dormant={run.submitDormant || undefined}
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
          {onClose && (
            <button
              type="button"
              className="report-convo-close"
              aria-label="Close conversation"
              onClick={onClose}
            >
              ×
            </button>
          )}
        </div>
      </header>

      <div
        ref={scrollRef}
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- scrollable document column must be keyboard-focusable.
        tabIndex={0}
        aria-label={drawerOpen ? 'Conversation' : 'Conversation closed'}
        className="report-convo-scroll"
        onScroll={onAnyScroll}
      >
        {specCardId == null ? (
          <p className="report-convo-empty">
            Spec Agent is unavailable for this track.
          </p>
        ) : (
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
              <div
                key={`${entry.queued ? 'queued' : 'item'}:${entry.id}`}
                data-chat-entry-id={entry.id}
              >
                <ConvoEntry
                  entry={entry}
                  expanded={expandedEntries.has(entry.id)}
                  onToggleExpanded={toggleExpandedEntry}
                />
              </div>
            ))}

            {isWorking && <ConvoTypingIndicator />}
          </div>
        )}
      </div>

      {specCardId != null && (
        <footer className="report-convo-inputbar">
          <div className="report-convo-inputbar-inner">
            {(run.submitError ?? run.stopError) != null && (
              <p
                className="report-convo-error"
                data-dormant={
                  (run.submitError != null && run.submitDormant) || undefined
                }
                role="alert"
              >
                {run.submitError ?? run.stopError}
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
              {/* While a turn is Working the send-glyph position becomes a
                  stop affordance (#668) — even with a non-empty draft, ■
                  wins (Enter still queues the draft). */}
              {drawerOpen && stopVisible ? (
                <button
                  type="button"
                  className="report-convo-send"
                  aria-label="Stop turn"
                  disabled={stopDisabled}
                  onClick={() => {
                    void onStop();
                  }}
                >
                  &#9632;
                </button>
              ) : (
                draft.trim() !== '' && (
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
                )
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
              The track report is preserved, but the spec conversation transcript
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
    </div>
  );
}
