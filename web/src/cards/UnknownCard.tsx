// Fallback rendering for kernel cards the registry couldn't adapt.
//
// `adaptKernelCard` returns null when no `CardEntry.fromKernel` claims a
// kernel card — usually because the card's `kind` is unrecognized or its
// `payload` failed the kind's zod schema. Before this slot existed,
// `currentWave` filtered the nulls and the wave just rendered with one
// fewer panel, which made schema drift invisible to the user. Now we
// surface a small placeholder so the user (and devtools) can see that
// something arrived from the server that the UI didn't know how to draw.
//
// The placeholder is intentionally minimal: no per-kind branching, no
// CSS additions. The head now goes through the typed `<CardHead>` slot
// component (issue #207) — the `card-drag-handle` class rides on the
// head root so RGL drag-handle scoping still works, and the title
// inherits the unified `.card-head-title` typography.

import type { CardSize } from './registry';
import { MONO_STACK } from '../font-stack';
import { CardHead } from './CardHead';

/** Same mid-range default the registry uses for unknown built-ins. */
export const UNKNOWN_CARD_SIZE: CardSize = { w: 4, h: 6, minW: 3, minH: 3 };

export function UnknownCard({ kernelKind }: { kernelKind: string }) {
  return (
    <div
      className="card-unknown"
      style={{
        border: '1px dashed var(--hairline-strong)',
        height: '100%',
        boxSizing: 'border-box',
        fontSize: 13,
        opacity: 0.75,
        display: 'flex',
        flexDirection: 'column',
      }}
    >
      <CardHead className="card-drag-handle" title="Unknown card" />
      <div
        style={{
          padding: 8,
          display: 'flex',
          flexDirection: 'column',
          gap: 6,
        }}
      >
        <code
          style={{
            fontFamily: MONO_STACK,
            fontSize: 12,
          }}
        >
          {kernelKind}
        </code>
        <small style={{ opacity: 0.7 }}>
          UI couldn't parse this card's payload.
        </small>
      </div>
    </div>
  );
}
