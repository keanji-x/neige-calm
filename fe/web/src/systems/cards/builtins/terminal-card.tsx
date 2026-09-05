// Copied from web/src/cards/builtins/terminal.tsx chrome: `.term` + CardHead
// + `.term-body`. The PTY renderer stays in systems/terminal.

import { Suspense, useCallback, useEffect } from 'react';

import { useState } from '../../../ui/state/public.ts';
import type { TerminalConnectionStatus } from '../../terminal/xterm-view.tsx';
import { TerminalSurface } from '../../terminal/surface.tsx';
import type { CardHostCapabilities } from '../contracts.ts';
import { PathLabel } from '../../../ui/path-label/public.tsx';
import { CardHead } from '../ui/card-head.tsx';
import type { WorkerSessionState } from '../../../../../core/api/schemas.js';

export function TerminalCardView({ card, host, onRemove, fallbackTitle = 'terminal' }: {
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
  const live = attached && !ended && status === 'connected';
  const message = card.sessionState === 'failed' ? 'Session failed.'
    : card.sessionState === 'superseded' ? 'Session replaced.'
      : status === 'exited' || card.sessionState === 'exited' ? 'Session exited.'
        : card.sessionState === 'starting' ? `Starting ${fallbackTitle}…`
          : 'No terminal session available.';
  return (
    <div
      className={attached ? 'term live' : 'term'}
      data-nc-terminal-card=""
      data-nc-terminal-id={card.terminalId ?? ''}
    >
      <CardHead
        className="card-drag-handle"
        title={card.title || fallbackTitle}
        status={live ? <span className="live-dot" role="img" aria-label="status Working" />
          : ended ? <span role="status">{message}</span>
            : attached ? <span role="status">{status === 'closed' ? 'Disconnected'
              : status === 'protocol-error' ? 'Connection error' : 'Connecting…'}</span> : undefined}
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
              <TerminalSurface card={card} visible={visible} onStatusChange={onStatusChange} />
            </Suspense>
          )
          : <div className="term-line">{ended ? 'No terminal session available.' : message}</div>}
      </div>
    </div>
  );
}
