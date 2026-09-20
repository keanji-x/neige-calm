import { Suspense, useCallback, useEffect } from 'react';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { TerminalSurface, type TerminalConnectionStatus } from '../../terminal/surface.tsx';
import type { CardHostCapabilities } from '../contracts.ts';
import { PathLabel } from '../../../ui/path-label/public.tsx';
import { CardHead } from '../ui/card-head.tsx';
import type { WorkerSessionState } from '../../../../../core/api/schemas.js';
import { activityLabelOf, cardActivityState, type CardActivity } from '../../../../../core/domain/activity.js';

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
  onRemove?: () => void;
  /** The head's indicator comes from this alone — not `card.sessionState` (alive while a process exists) nor the surface's connection status (this tab's attachment). */
  activity: CardActivity | null;
  /** Claude and codex worker cards share this renderer and must not announce themselves as "terminal". */
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
        status={activity === null && statusText === null ? undefined : <>
          {activity !== null && (
            <ActivityIndicator state={cardActivityState(activity)} spoken={activityLabelOf(cardActivityState(activity))} />
          )}
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
