// Proposal preview state machine (issue #955 §5.2.1 — "顺序校验-应用").
//
// A proposal is an ORDERED list of ops applied in one transaction: each op
// is validated and applied against the document produced by the ops before
// it, not against the live document. Rendering every op against a single
// immutable snapshot of the live report is therefore a lie the adjudicator
// cannot detect — `upsert(b1, new)` followed by `move(b1, …)` would show
// the move carrying b1's OLD payload, and an earlier `delete b1` would not
// stop a later `move b2 after b1` from looking anchored.
//
// So we walk the ops here, maintaining a proposal-local simulated document
// (creations with their temp ids, deletions, moves, rev bumps), and hand
// the renderer a per-op before/after view computed against that state.
// The simulation deliberately mirrors the kernel's lowering rules in
// `crates/calm-server/src/wave_report_proposal/mod.rs`:
//
//   * creation position = 0 (at_start) / len (at_end) / anchor index + 1
//   * move target index  = anchor index shifted left by one when the
//     anchor sat after the moved block, then + 1
//   * a replace bumps the block's rev by one unless the content is
//     byte-identical (`upsert_block`'s idempotent-replace path)
//
// Three states, not two (§5.6 honesty): a block can be PRESENT, provably
// ABSENT, or UNKNOWN. Unknown is what we have when the page holds no block
// index at all — a body-only report, no report card, or an unsupported
// payload version. Unknown must never be narrated as "no longer in the
// report", and it must not fire the advisory staleness hint: we have not
// compared anything.

import type {
  ProposalAnchor,
  ProposalOp,
  ProposalUpsertOp,
} from '../../api/wire';
import type { ReportBlock } from '../../cards/builtins/wave-report';

const TEMP_REF_PREFIX = 'temp:';

const GONE = 'This block is no longer in the report.';
const UNKNOWN =
  'The report blocks are not loaded here, so this block cannot be shown.';

/** One rendered side of one op. */
export interface PaneView {
  /** The block to preview, when we have one. */
  block?: ReportBlock;
  /** Shown instead of a block. */
  placeholder?: string;
  /** Intent line (anchor / move destination). */
  note?: string;
  /** Position line, omitted when the position is not knowable. */
  position?: string;
}

export interface OpView {
  headline: string;
  /**
   * Advisory only (§5.6). `true` = this op's anchors no longer match the
   * simulated state, `false` = they do, `null` = we hold no block index,
   * so there is nothing to compare and no hint to show.
   */
  stale: boolean | null;
  before: PaneView;
  after: PaneView;
}

export function anchorLabel(anchor: ProposalAnchor): string {
  if (anchor === 'at_start') return 'at the top of the report';
  if (anchor === 'at_end') return 'at the end of the report';
  const target = anchor.after_block_id;
  return target.startsWith(TEMP_REF_PREFIX)
    ? `after the block created earlier in this proposal (${target.slice(
        TEMP_REF_PREFIX.length,
      )})`
    : `after ${target}`;
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** A payload that is not a JSON object cannot be a block payload, so it
 *  degrades to the renderer's "unsupported block" placeholder rather than
 *  throwing. */
function payloadOf(op: ProposalUpsertOp): Record<string, unknown> {
  return isRecord(op.payload) ? op.payload : {};
}

interface SimBlock {
  id: string;
  rev: number;
  kind: string;
  payload: Record<string, unknown>;
}

function asBlock(b: SimBlock): ReportBlock {
  return { id: b.id, rev: b.rev, kind: b.kind, payload: b.payload };
}

function positionLine(i: number, total: number): string {
  return `Position ${i + 1} of ${total}`;
}

/**
 * Walk `ops` in order against a simulation of `blocks`, returning one
 * before/after view per op.
 *
 * `blocks === undefined` means "the page has no block index" — every pane
 * comes back UNKNOWN and every `stale` comes back `null`.
 */
export function simulateProposal(
  ops: ProposalOp[],
  blocks: ReportBlock[] | undefined,
): OpView[] {
  const known = blocks != null;
  const doc: SimBlock[] = (blocks ?? []).map((b) => ({
    id: b.id,
    rev: b.rev,
    kind: b.kind,
    payload: b.payload,
  }));
  /** temp_id → the simulated id of the block that op minted. */
  const minted = new Map<string, string>();
  const views: OpView[] = [];

  const indexOf = (id: string) => doc.findIndex((b) => b.id === id);

  /** Resolve an anchor to a document id, translating `temp:` refs. */
  const anchorTarget = (anchor: ProposalAnchor): string | null => {
    if (anchor === 'at_start' || anchor === 'at_end') return null;
    const target = anchor.after_block_id;
    if (!target.startsWith(TEMP_REF_PREFIX)) return target;
    return minted.get(target.slice(TEMP_REF_PREFIX.length)) ?? null;
  };

  const anchorIndex = (anchor: ProposalAnchor): number => {
    const target = anchorTarget(anchor);
    return target == null ? -1 : indexOf(target);
  };

  const anchorResolves = (anchor: ProposalAnchor | null | undefined): boolean =>
    anchor == null ||
    anchor === 'at_start' ||
    anchor === 'at_end' ||
    anchorIndex(anchor) >= 0;

  /** Before pane for an op whose target is missing from the simulation. */
  const missingPane = (): PaneView => ({ placeholder: known ? GONE : UNKNOWN });

  for (const op of ops) {
    const headline = opHeadline(op);

    // ---- creation (upsert with temp_id) --------------------------------
    if (op.op === 'upsert_block' && op.block_id == null) {
      const resolves = anchorResolves(op.anchor);
      let at = doc.length;
      if (op.anchor === 'at_start') at = 0;
      else if (op.anchor != null && op.anchor !== 'at_end') {
        const ti = anchorIndex(op.anchor);
        at = ti >= 0 ? ti + 1 : doc.length;
      }
      const id = `${TEMP_REF_PREFIX}${op.temp_id ?? 'new'}`;
      const payload = payloadOf(op);
      views.push({
        headline,
        stale: known ? !resolves : null,
        before: { placeholder: 'No existing block — this one is new.' },
        after: {
          block: { id, rev: 1, kind: op.kind, payload },
          note: op.anchor ? `Insert ${anchorLabel(op.anchor)}` : undefined,
          position:
            known && resolves ? positionLine(at, doc.length + 1) : undefined,
        },
      });
      doc.splice(at, 0, { id, rev: 1, kind: op.kind, payload });
      if (op.temp_id != null) minted.set(op.temp_id, id);
      continue;
    }

    // ---- replace (upsert with block_id) --------------------------------
    if (op.op === 'upsert_block') {
      const id = op.block_id as string;
      const i = indexOf(id);
      const live = i >= 0 ? doc[i] : undefined;
      const payload = payloadOf(op);
      // Mirror `ReportDoc::upsert_block`: identical content is an
      // idempotent replace and does NOT bump the rev.
      const identical =
        live != null &&
        live.kind === op.kind &&
        JSON.stringify(live.payload) === JSON.stringify(payload);
      const rev =
        live == null
          ? (op.if_rev ?? 0)
          : identical
            ? live.rev
            : live.rev + 1;
      views.push({
        headline,
        stale: known
          ? live == null || (op.if_rev != null && live.rev !== op.if_rev)
          : null,
        before:
          live != null
            ? { block: asBlock(live), position: positionLine(i, doc.length) }
            : missingPane(),
        after: {
          block: { id, rev, kind: op.kind, payload },
          position:
            live != null ? positionLine(i, doc.length) : undefined,
        },
      });
      if (live != null) doc[i] = { id, rev, kind: op.kind, payload };
      continue;
    }

    // ---- move ----------------------------------------------------------
    if (op.op === 'move_block') {
      const i = indexOf(op.block_id);
      const live = i >= 0 ? doc[i] : undefined;
      const resolves = anchorResolves(op.anchor);
      let to = i;
      if (live != null && resolves) {
        if (op.anchor === 'at_start') to = 0;
        else if (op.anchor === 'at_end') to = doc.length - 1;
        else {
          const ti = anchorIndex(op.anchor);
          // `move_block` removes the block first, so an anchor sitting
          // after it shifts left by one.
          to = ti > i ? ti : ti + 1;
        }
      }
      const moveNote = `Moved ${anchorLabel(op.anchor)}`;
      views.push({
        headline,
        stale: known
          ? live == null || !resolves || live.rev !== op.if_rev
          : null,
        before:
          live != null
            ? { block: asBlock(live), position: positionLine(i, doc.length) }
            : missingPane(),
        after:
          live != null
            ? {
                block: asBlock(live),
                note: moveNote,
                position: resolves ? positionLine(to, doc.length) : undefined,
              }
            : { ...missingPane(), note: moveNote },
      });
      if (live != null && resolves && to !== i) {
        doc.splice(i, 1);
        doc.splice(to, 0, live);
      }
      continue;
    }

    // ---- delete ----------------------------------------------------------
    const i = indexOf(op.block_id);
    const live = i >= 0 ? doc[i] : undefined;
    views.push({
      headline,
      stale: known ? live == null || live.rev !== op.if_rev : null,
      before:
        live != null
          ? { block: asBlock(live), position: positionLine(i, doc.length) }
          : missingPane(),
      after: { placeholder: 'Block removed.' },
    });
    if (live != null) doc.splice(i, 1);
  }

  return views;
}
