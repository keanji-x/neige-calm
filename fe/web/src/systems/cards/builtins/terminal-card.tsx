// Copied from web/src/cards/builtins/terminal.tsx chrome: `.term` + CardHead
// + `.term-body`. The PTY renderer stays in systems/terminal.

import { Suspense } from 'react';

import { TerminalSurface } from '../../terminal/surface.tsx';
import { CardHead } from '../ui/card-head.tsx';

export function TerminalCardView({ card }: {
  card: { readonly id: string; readonly title: string | null; readonly terminalId: string | null };
}) {
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
              <TerminalSurface card={card} />
            </Suspense>
          )
          : <div className="term-line">Starting terminal…</div>}
      </div>
    </div>
  );
}
