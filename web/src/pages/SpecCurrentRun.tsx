import { useEffect, useRef } from 'react';
import { useState } from '../shared/state';
import { ConfirmDialog } from '../ui/ConfirmDialog/ConfirmDialog';
import {
  humanizeToken,
  useSpecCurrentRun,
} from './useSpecCurrentRun';

export interface SpecCurrentRunProps {
  /** Spec card id; null/undefined when wave has no spec card. */
  specCardId: string | null;
}

export function SpecCurrentRun({ specCardId }: SpecCurrentRunProps) {
  const run = useSpecCurrentRun(specCardId ?? undefined);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState('');
  const [resetOpen, setResetOpen] = useState(false);
  const [resetAttempted, setResetAttempted] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const latestSpecCardIdRef = useRef<string | null>(specCardId);
  latestSpecCardIdRef.current = specCardId;

  useEffect(() => {
    if (!open) return;
    const id = window.setTimeout(() => textareaRef.current?.focus(), 30);
    return () => window.clearTimeout(id);
  }, [open]);

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
      setDraft('');
      setOpen(false);
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

          <div className="report-chat-tool" aria-label="Latest tool">
            <span className="report-chat-tool-label">
              {run.latestTool.toolLabel ?? 'No active tool'}
            </span>
            {run.latestTool.toolStatus && (
              <span className="report-chat-tool-status">
                {humanizeToken(run.latestTool.toolStatus)}
              </span>
            )}
          </div>

          <div className="report-chat-input">
            <textarea
              ref={textareaRef}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  void onSubmit();
                }
              }}
              placeholder="Ask a follow-up about this report..."
              rows={1}
              disabled={run.submitPending || run.resetPending}
              aria-label="Follow-up"
            />
            <button
              type="button"
              className="report-chat-send"
              aria-label="Send"
              disabled={!draft.trim() || run.submitPending || run.resetPending}
              onClick={() => void onSubmit()}
            >
              Send
            </button>
          </div>

          {run.submitError && (
            <p className="report-chat-error" role="alert">
              {run.submitError}
            </p>
          )}

          <footer className="report-chat-footer">
            <button
              type="button"
              className="report-chat-reset"
              onClick={() => {
                setResetAttempted(false);
                setResetOpen(true);
              }}
            >
              Reset session...
            </button>
          </footer>
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
