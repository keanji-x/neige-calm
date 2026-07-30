// Before/after rendering of one proposed block op (issue #955 §5.5 —
// "UI 以 #960 D3 的『改前块 / 改后块』并排呈现").
//
// The two panes render through `ReportBlockView`, the SAME renderer the
// report document uses, so a proposed `chart.candles` / `table` / `app`
// block is previewed exactly as it will look once accepted. Per #960 D3
// non-prose blocks are compared as whole blocks — there is deliberately
// no parameter-level diff.
//
// Which pane exists depends on the op (§5.2.1):
//
//   upsert + block_id  →  before = the live block, after = the proposal's
//   upsert + temp_id   →  no before (the block does not exist yet)
//   move_block         →  both panes hold the same block; the position
//                         line under each is what differs
//   delete_block       →  no after
//
// Staleness shown here is ADVISORY ONLY (§5.6): it compares each op's
// `if_rev` against the block revs the page currently holds. The
// authoritative verdict is decided inside the accept transaction, so this
// hint never disables the accept button. `base_doc_heads` is not checkable
// client-side at all — the report card payload carries no heads token —
// which is a second reason the hint cannot be treated as authoritative.

import type {
  ProposalAnchor,
  ProposalOp,
  ProposalUpsertOp,
} from '../../api/wire';
import type { ReportBlock } from '../../cards/builtins/wave-report';
import { ReportBlockView } from '../report-blocks';

/** Live blocks keyed by id, with their 1-based position in the document. */
export interface LiveBlock {
  block: ReportBlock;
  position: number;
  total: number;
}
export type BlockIndex = Map<string, LiveBlock>;

export function indexBlocks(blocks: ReportBlock[] | undefined): BlockIndex {
  const list = blocks ?? [];
  const index: BlockIndex = new Map();
  list.forEach((block, i) => {
    index.set(block.id, { block, position: i + 1, total: list.length });
  });
  return index;
}

export function anchorLabel(anchor: ProposalAnchor): string {
  if (anchor === 'at_start') return 'at the top of the report';
  if (anchor === 'at_end') return 'at the end of the report';
  const target = anchor.after_block_id;
  return target.startsWith('temp:')
    ? `after the block created earlier in this proposal (${target.slice(5)})`
    : `after ${target}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** The proposed block, shaped for `ReportBlockView`. A payload that is
 *  not a JSON object cannot be a block payload, so it degrades to the
 *  renderer's "unsupported block" placeholder rather than throwing. */
export function proposedBlock(op: ProposalUpsertOp): ReportBlock {
  return {
    id: op.block_id ?? `temp:${op.temp_id ?? 'new'}`,
    rev: op.if_rev ?? 0,
    kind: op.kind,
    payload: isRecord(op.payload) ? op.payload : {},
  };
}

/** The block id an op targets, or `null` for a creation. */
export function opBlockId(op: ProposalOp): string | null {
  if (op.op === 'upsert_block') return op.block_id ?? null;
  return op.block_id;
}

/**
 * Advisory only (§5.6). `true` means "the page's current blocks no longer
 * match what this op anchors against" — the block vanished, its rev moved,
 * or the op's anchor names a block that is gone. Never gates accept.
 */
export function opMayBeStale(op: ProposalOp, index: BlockIndex): boolean {
  const anchor = op.op === 'delete_block' ? null : op.anchor;
  if (
    anchor != null &&
    anchor !== 'at_start' &&
    anchor !== 'at_end' &&
    !anchor.after_block_id.startsWith('temp:') &&
    !index.has(anchor.after_block_id)
  ) {
    return true;
  }
  const id = opBlockId(op);
  if (id == null) return false;
  const live = index.get(id);
  if (!live) return true;
  return op.if_rev != null && live.block.rev !== op.if_rev;
}

export function opHeadline(op: ProposalOp): string {
  switch (op.op) {
    case 'upsert_block':
      return op.block_id != null
        ? `Modify ${op.kind} block ${op.block_id}`
        : `New ${op.kind} block`;
    case 'move_block':
      return `Move block ${op.block_id}`;
    case 'delete_block':
      return `Delete block ${op.block_id}`;
  }
}

function positionLine(live: LiveBlock | undefined): string {
  return live ? `Position ${live.position} of ${live.total}` : 'Not in the report';
}

function Pane({
  side,
  headline,
  note,
  block,
  placeholder,
}: {
  side: 'Before' | 'After';
  headline: string;
  note?: string;
  block?: ReportBlock;
  placeholder?: string;
}) {
  return (
    <section
      className={'pp-pane' + (side === 'After' ? ' pp-pane--after' : '')}
      aria-label={`${side} — ${headline}`}
    >
      <h5 className="pp-pane-head">{side}</h5>
      {block ? (
        <div className="pp-pane-body">
          <ReportBlockView block={block} />
        </div>
      ) : (
        <p className="pp-pane-empty">{placeholder}</p>
      )}
      {note != null && <p className="pp-pane-note">{note}</p>}
    </section>
  );
}

export function ProposalOpDiff({
  op,
  index,
}: {
  op: ProposalOp;
  index: BlockIndex;
}) {
  const headline = opHeadline(op);
  const targetId = opBlockId(op);
  const live = targetId != null ? index.get(targetId) : undefined;
  const stale = opMayBeStale(op, index);

  return (
    <li className="pp-op">
      <div className="pp-op-head">
        <span className="pp-op-title">{headline}</span>
        {stale && (
          <span
            className="pp-op-hint"
            title="Advisory only — the report has changed since this proposal was written. The authoritative check happens when you accept."
          >
            may be out of date
          </span>
        )}
      </div>
      <div className="pp-op-panes">
        {op.op === 'upsert_block' && op.block_id == null ? (
          <Pane
            side="Before"
            headline={headline}
            placeholder="No existing block — this one is new."
            note={op.anchor ? `Insert ${anchorLabel(op.anchor)}` : undefined}
          />
        ) : (
          <Pane
            side="Before"
            headline={headline}
            block={live?.block}
            placeholder="This block is no longer in the report."
            note={positionLine(live)}
          />
        )}
        {op.op === 'delete_block' ? (
          <Pane
            side="After"
            headline={headline}
            placeholder="Block removed."
          />
        ) : op.op === 'move_block' ? (
          <Pane
            side="After"
            headline={headline}
            block={live?.block}
            placeholder="This block is no longer in the report."
            note={`Moved ${anchorLabel(op.anchor)}`}
          />
        ) : (
          <Pane
            side="After"
            headline={headline}
            block={proposedBlock(op)}
            note={
              op.anchor && op.block_id == null
                ? `Insert ${anchorLabel(op.anchor)}`
                : undefined
            }
          />
        )}
      </div>
    </li>
  );
}
