// Copied from web/src/cards/builtins/terminal.tsx chrome: `.term` + CardHead
// + `.term-body`. The PTY renderer stays in systems/terminal.

import { Suspense, useEffect } from 'react';

import { useState } from '../../../ui/state/public.ts';
import { TerminalSurface } from '../../terminal/surface.tsx';
import type { CardHostCapabilities } from '../contracts.ts';
import { CardHead } from '../ui/card-head.tsx';

export function TerminalCardView({ card, host }: {
  card: { readonly id: string; readonly title: string | null; readonly terminalId: string | null };
  host: CardHostCapabilities;
}) {
  const [visible, setVisible] = useState(() => host.lifecycle.getSnapshot().visible);
  useEffect(() => host.lifecycle.subscribe(() => {
    setVisible(host.lifecycle.getSnapshot().visible);
  }), [host]);
  const live = card.terminalId !== null;
  return (
    <div
      className={live ? 'term live' : 'term'}
      data-nc-terminal-card=""
      data-nc-terminal-id={card.terminalId ?? ''}
    >
      <CardHead
        className="card-drag-handle"
        title={card.title || 'terminal'}
        status={live ? <span className="live-dot" role="img" aria-label="status Working" /> : undefined}
      />
      <div className="term-body">
        {live
          ? (
            <Suspense fallback={<div className="term-line">Loading terminal…</div>}>
              <TerminalSurface card={card} visible={visible} />
            </Suspense>
          )
          : <div className="term-line">Starting terminal…</div>}
      </div>
    </div>
  );
}
