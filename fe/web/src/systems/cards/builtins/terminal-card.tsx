import { CardFrame } from '../../../ui/card-frame/public.tsx';
import { Suspense, useCallback, useEffect, type ReactNode } from 'react';
import { Banner } from '@astryxdesign/core/Banner';

import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { TerminalSurface, type TerminalConnectionStatus } from '../../terminal/surface.tsx';
import type { CardHostCapabilities } from '../contracts.ts';
import { PathLabel } from '../../../ui/path-label/public.tsx';
import type { CardHeaderPresentation } from '../registry.js';
import { CardHead } from '../ui/card-head.tsx';
import type { WorkerSessionState } from '../../../../../core/api/schemas.js';
import { activityLabelOf, cardActivityState, type CardActivity } from '../../../../../core/domain/activity.js';

export function TerminalCardView({
  card, host, onRemove, activity, notice, fallbackTitle = 'terminal', headActions = null, attachKey = null, header,
}: {
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
  header?: CardHeaderPresentation;
  /** The head's indicator comes from this alone — not `card.sessionState` (alive while a process exists) nor the surface's connection status (this tab's attachment). */
  activity: CardActivity | null;
  /** The composer's state line, shown as given above the terminal; it never gates input. */
  notice: string | null;
  /** Claude and codex worker cards share this renderer and must not announce themselves as "terminal". */
  fallbackTitle?: string;
  /** The head's own actions, at its right end beside the status (a Claude card's Update or Resume). */
  headActions?: ReactNode;
  /** A new value mounts a fresh terminal attach: a restarted child on the same terminal (a Claude card's new runtime). */
  attachKey?: string | null;
}) {
  const [focused, setFocused] = useState(() => host.lifecycle.getSnapshot().focused);
  const [visible, setVisible] = useState(() => host.lifecycle.getSnapshot().visible);
  useEffect(() => {
    const sync = () => {
    setVisible(host.lifecycle.getSnapshot().visible);
    setFocused(host.lifecycle.getSnapshot().focused);
    };
    sync();
    return host.lifecycle.subscribe(sync);
  }, [host]);
  const [connection, setConnection] = useState<Readonly<{
    terminalId: string | null; attachKey: string | null; status: TerminalConnectionStatus;
  }>>({ terminalId: card.terminalId, attachKey, status: 'connecting' });
  const terminalId = card.terminalId;
  const onStatusChange = useCallback((status: TerminalConnectionStatus) => {
    setConnection({ terminalId, attachKey, status });
  }, [terminalId, attachKey]);
  const status = connection.terminalId === terminalId && connection.attachKey === attachKey ? connection.status : 'connecting';
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
    <CardFrame
      className={attached ? 'term live' : 'term'}
      data-nc-terminal-card=""
      data-nc-terminal-id={card.terminalId ?? ''}
    >
      <CardHead
        className="card-drag-handle"
        title={card.title || fallbackTitle}
        status={activity === null && statusText === null && headActions === null ? undefined : <>
          {activity !== null && (
            <ActivityIndicator state={cardActivityState(activity)} spoken={activityLabelOf(cardActivityState(activity))} />
          )}
          {statusText !== null && <span role="status">{statusText}</span>}
          {headActions}
        </>}
        onDelete={onRemove}
        deleteLabel={`Delete card ${card.title || fallbackTitle}`}
        header={header}
      />
      {card.cwd !== null && (
        <PathLabel label="Working directory" path={card.cwd} />
      )}
      {card.gateCwd !== null && (
        <PathLabel label="Gate working directory" path={card.gateCwd} />
      )}
      {notice !== null && (
        <Banner status="info" container="section" title={notice} data-nc-terminal-notice="" />
      )}
      <div className="term-body">
        {attached
          ? (
            <Suspense fallback={<div className="term-line">Loading terminal…</div>}>
              <TerminalSurface key={attachKey ?? ''} recovery={host.recovery} card={card} visible={visible} focused={focused} onStatusChange={onStatusChange} />
            </Suspense>
          )
          : <div className="term-line">{ended ? 'No terminal session available.' : message}</div>}
      </div>
    </CardFrame>
  );
}
