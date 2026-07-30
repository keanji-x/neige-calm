// What a proposal pane is allowed to RENDER (issue #955 §5.4/§5.6).
//
// ─────────────────────────────────────────────────────────────────────────
// SECURITY INVARIANT — do not "restore" live rendering here.
//
// A pending proposal is UNADJUDICATED plugin content. The whole point of
// the ④ channel is that a plugin cannot write the report; a human decides.
// §5.4 backs that with two in-kernel role_gate clauses making
// `ProposalResolved{accepted}` User-only.
//
// The `app` block renderer (`../report-blocks/app.tsx`) mounts a live
// iframe with `sandbox="allow-scripts allow-same-origin"` pointed at a
// same-origin path — and `/api/plugins/{id}/resources/{view_id}` serves
// PLUGIN-AUTHORED HTML on that origin. Rendering a proposed (or
// about-to-be-deleted) `app` block through it would mean: submitting a
// proposal is enough to run plugin JS with the user's session, in the calm
// origin, the moment the user merely OPENS the report — including a
// scripted `POST /api/proposals/{id}/accept`. That is exactly the hole the
// channel exists to close, and it contradicts the panel's own promise
// ("change nothing until you accept them").
//
// So: in the proposal panes an `app` block renders as a STATIC,
// NON-EXECUTING descriptor — kind, src, and its declared parameters — in
// BOTH panes (a proposed delete or move of an existing app block must not
// live-mount it either). Weakening this to "drop allow-same-origin" is not
// equivalent: no plugin-controlled document should load in this path at
// all. Every other block kind is inert markup/canvas and goes through the
// shared `ReportBlockView`.
// ─────────────────────────────────────────────────────────────────────────

import type { ReportBlock } from '../../cards/builtins/wave-report';
import { ReportBlockView } from '../report-blocks';

function asString(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function AppBlockDescriptor({ block }: { block: ReportBlock }) {
  const payload: Record<string, unknown> = block.payload;
  const src = asString(payload.src);
  const title = asString(payload.title);
  const height = typeof payload.height === 'number' ? payload.height : null;

  return (
    <div className="report-block pp-app" role="note">
      <p className="pp-app-head">app block — not run in this preview</p>
      <dl className="pp-app-params">
        <dt>src</dt>
        <dd>
          <code>{src ?? '(missing)'}</code>
        </dd>
        {title != null && (
          <>
            <dt>title</dt>
            <dd>{title}</dd>
          </>
        )}
        {height != null && (
          <>
            <dt>height</dt>
            <dd>{height}px</dd>
          </>
        )}
      </dl>
      <p className="pp-app-why">
        The embedded app runs only after you accept this proposal.
      </p>
    </div>
  );
}

/** The renderer every proposal pane must use. */
export function ProposalBlockPreview({ block }: { block: ReportBlock }) {
  if (block.kind === 'app') return <AppBlockDescriptor block={block} />;
  return <ReportBlockView block={block} />;
}
