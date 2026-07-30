// Before/after rendering of one proposed block op (issue #955 §5.5 —
// "UI 以 #960 D3 的『改前块 / 改后块』并排呈现").
//
// This file is pure presentation. WHAT each pane holds is decided by
// `simulate.ts`, which walks the ops in order against a proposal-local
// simulated document (§5.2.1 sequential apply); WHICH renderer a pane may
// use is decided by `blockPreview.tsx` (an unadjudicated `app` block is
// never live-mounted — read the invariant comment there before touching
// it).
//
// Which pane exists depends on the op (§5.2.1):
//
//   upsert + block_id  →  before = the simulated block, after = proposed
//   upsert + temp_id   →  no before (the block does not exist yet)
//   move_block         →  both panes hold the same block; the position
//                         line under each is what differs
//   delete_block       →  no after
//
// Staleness shown here is ADVISORY ONLY (§5.6): it compares each op's
// `if_rev` / anchors against the simulated state. The authoritative
// verdict is decided inside the accept transaction, so this hint never
// disables the accept button — and it is stated as advisory in VISIBLE
// text, not a hover-only `title`. `base_doc_heads` is not checkable
// client-side at all — the report card payload carries no heads token —
// which is a second reason the hint cannot be treated as authoritative.

import { ProposalBlockPreview } from './blockPreview';
import { NOT_SIMULATED_NOTE, type OpView, type PaneView } from './simulate';

function Pane({
  side,
  headline,
  pane,
}: {
  side: 'Before' | 'After';
  headline: string;
  pane: PaneView;
}) {
  return (
    <section
      className={'pp-pane' + (side === 'After' ? ' pp-pane--after' : '')}
      aria-label={`${side} — ${headline}`}
    >
      {/* h3 under the panel's h2 — no heading-level skip. */}
      <h3 className="pp-pane-head">{side}</h3>
      {pane.block ? (
        <div className="pp-pane-body">
          <ProposalBlockPreview block={pane.block} />
        </div>
      ) : (
        <p className="pp-pane-empty">{pane.placeholder}</p>
      )}
      {pane.note != null && <p className="pp-pane-note">{pane.note}</p>}
      {pane.position != null && <p className="pp-pane-note">{pane.position}</p>}
    </section>
  );
}

export function ProposalOpDiff({ view }: { view: OpView }) {
  return (
    <li className="pp-op">
      <div className="pp-op-head">
        <span className="pp-op-title">{view.headline}</span>
        {/* Structural ≠ stale (§5.2.1). "Out of date" promises a re-read
            fixes it; a proposal that contradicts itself can never apply,
            and the kernel answers 400 (the proposal stays pending). Say
            the right cause so the user rejects instead of waiting. */}
        {view.structural != null && (
          <>
            <span className="pp-op-hint pp-op-hint--blocked">
              cannot be applied
            </span>
            <span className="pp-op-hint-note">{view.structural}</span>
          </>
        )}
        {/* The batch is atomic: nothing after the first failed op runs, so
            we do not pretend to know what this one would look like. */}
        {view.notSimulated === true && (
          <>
            <span className="pp-op-hint pp-op-hint--blocked">not previewed</span>
            <span className="pp-op-hint-note">{NOT_SIMULATED_NOTE}</span>
          </>
        )}
        {/* `stale === null` means "we hold no block index" — unknown is
            not the same as out of date, so no hint at all. */}
        {view.stale === true && (
          <>
            <span className="pp-op-hint">may be out of date</span>
            <span className="pp-op-hint-note">
              Advisory only — the report changed since this proposal was
              written; the authoritative check happens when you accept.
            </span>
          </>
        )}
      </div>
      <div className="pp-op-panes">
        <Pane side="Before" headline={view.headline} pane={view.before} />
        <Pane side="After" headline={view.headline} pane={view.after} />
      </div>
    </li>
  );
}
