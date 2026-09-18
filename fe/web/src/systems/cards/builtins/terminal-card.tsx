// Copied from web/src/cards/builtins/terminal.tsx chrome: `.term` + CardHead
// + `.term-body`. The PTY renderer stays in systems/terminal.

import { Suspense, useCallback, useEffect } from 'react';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { TerminalSurface, type TerminalConnectionStatus } from '../../terminal/surface.tsx';
import type { CardHostCapabilities } from '../contracts.ts';
import { PathLabel } from '../../../ui/path-label/public.tsx';
import { CardHead } from '../ui/card-head.tsx';
import type { WorkerSessionState } from '../../../../../core/api/schemas.js';
import { cardActivityState, type CardActivity } from '../../../../../core/domain/activity.js';

export function TerminalCardView({ card, host, onRemove, activity, fallbackTitle = 'terminal' }: {
  card: {
    readonly id: string;
    readonly title: string | null;
    readonly terminalId: string | null;
    readonly sessionState: WorkerSessionState | null;
    readonly cwd: string | null;
    readonly gateCwd: string | null;
  };
  host: CardHostCapabilities;
  /** The board's delete, already resolved — see `CardComponentProps.onRemove`. */
  onRemove?: () => void;
  /**
   * The kernel's verdict for this card, already resolved — see
   * `CardComponentProps.activity`. The head's indicator comes from this and
   * nothing else (INV-APP-118): not from `card.sessionState`, which is the
   * session reading (`runtime.status`) that is `running` for as long as a
   * process exists, and not from the surface's connection status, which says
   * whether *this tab* is attached. A process being alive and a socket being
   * open are both facts this head still prints as words, below.
   */
  activity: CardActivity | null;
  /**
   * Head label when the kernel row carries no title. Claude and codex worker
   * cards share this renderer (they are PTYs too) and must not announce
   * themselves as "terminal"; `LetterAvatar` also colours the avatar off this
   * string.
   */
  fallbackTitle?: string;
}) {
  const [visible, setVisible] = useState(() => host.lifecycle.getSnapshot().visible);
  useEffect(() => host.lifecycle.subscribe(() => {
    setVisible(host.lifecycle.getSnapshot().visible);
  }), [host]);
  const [connection, setConnection] = useState<Readonly<{
    terminalId: string | null; status: TerminalConnectionStatus;
  }>>({ terminalId: card.terminalId, status: 'connecting' });
  const terminalId = card.terminalId;
  const onStatusChange = useCallback((status: TerminalConnectionStatus) => {
    setConnection({ terminalId, status });
  }, [terminalId]);
  const status = connection.terminalId === terminalId ? connection.status : 'connecting';
  const attached = card.terminalId !== null;
  const ended = status === 'exited' || card.sessionState === 'exited' || card.sessionState === 'failed' || card.sessionState === 'superseded';
  const message = card.sessionState === 'failed' ? 'Session failed.'
    : card.sessionState === 'superseded' ? 'Session replaced.'
      : status === 'exited' || card.sessionState === 'exited' ? 'Session exited.'
        : card.sessionState === 'starting' ? `Starting ${fallbackTitle}…`
          : 'No terminal session available.';
  /* The head's words: how the session ended, else how this tab's connection
     stands — and nothing while it is simply connected. */
  const statusText = ended ? message
    : !attached ? null
      : status === 'closed' ? 'Disconnected'
        : status === 'protocol-error' ? 'Connection error'
          : status === 'connected' ? null : 'Connecting…';
  return (
    <div
      className={attached ? 'term live' : 'term'}
      data-nc-terminal-card=""
      data-nc-terminal-id={card.terminalId ?? ''}
    >
      <CardHead
        className="card-drag-handle"
        title={card.title || fallbackTitle}
        /* Two facts, side by side and from two sources: the kernel's verdict
           (the indicator) and the session / connection text. Neither stands
           in for the other — a failed session can be red AND say `Session
           failed.`, and a connected terminal with no verdict says nothing. */
        status={activity === null && statusText === null ? undefined : <>
          {activity !== null && <ActivityIndicator state={cardActivityState(activity)} />}
          {statusText !== null && <span role="status">{statusText}</span>}
        </>}
        onClose={onRemove}
        closeAriaLabel={`Delete card ${card.title || fallbackTitle}`}
      />
      {card.cwd !== null && (
        <PathLabel label="Working directory" path={card.cwd} />
      )}
      {card.gateCwd !== null && (
        <PathLabel label="Gate working directory" path={card.gateCwd} />
      )}
      <div className="term-body">
        {attached
          ? (
            <Suspense fallback={<div className="term-line">Loading terminal…</div>}>
              <TerminalSurface recovery={host.recovery} card={card} visible={visible} onStatusChange={onStatusChange} />
            </Suspense>
          )
          : <div className="term-line">{ended ? 'No terminal session available.' : message}</div>}
      </div>
    </div>
  );
}
