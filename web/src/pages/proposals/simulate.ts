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
//
// TWO more things the kernel does that this simulation must not diverge
// from, because diverging means showing the adjudicator a document the
// atomic batch can never produce:
//
//   1. ATOMICITY. `apply_proposal_batch` runs on a scratch fork and
//      returns at the FIRST op whose precondition fails; nothing after it
//      ever runs, and nothing is committed. So a simulation that keeps
//      mutating past a rejected op (installing a rev-mismatched replace,
//      minting an unresolvable creation) narrates a future that cannot
//      happen. We halt at the first failed authoritative precondition and
//      mark every later op NOT SIMULATED.
//
//   2. THE ERROR SPLIT (§5.2.1). The kernel classifies two different
//      causes and the remedies are opposite:
//        * STALE (`Conflict`) — the target/anchor is absent from the base
//          document, or `if_rev` moved. Snapshot decay: re-read the
//          report and re-propose and it can succeed.
//        * STRUCTURAL (`BadRequest`) — the proposal contradicts ITSELF:
//          it references a block an EARLIER op of the same proposal
//          deleted, anchors on a `temp:` id no earlier op creates, or
//          anchors a block after itself. `ensure_not_self_deleted` /
//          `resolve_anchor_index` in `wave_report_proposal/mod.rs`. No
//          re-read in any future makes it applicable, and a §5.6 400
//          leaves it PENDING. Narrating this as "may be out of date"
//          states the wrong cause and invites a pointless retry.
//
// Idempotence is compared on CANONICALIZED payloads, because the kernel
// compares canonical JSON content: `{a,b}` and `{b,a}` are the same
// document to it (no rev bump), and if we bumped the rev on key order
// alone, the NEXT op's `if_rev` would be falsely flagged out of date.

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
/** Pane text for a target an EARLIER op of this same proposal deleted. */
const SELF_DELETED =
  'An earlier change in this proposal deletes this block.';
/** Pane text for an op the halted simulation never reached. */
const NOT_SIMULATED = 'Not previewed.';

/** Head note for every op after the one that cannot be applied. */
export const NOT_SIMULATED_NOTE =
  'Not previewed — an earlier change in this proposal cannot be applied, and the whole proposal is applied as one transaction, so nothing after it would run.';

// The structural (kernel `BadRequest`) messages. Each says the same two
// things: the proposal contradicts itself, and re-reading cannot fix it.
const SELF_CONTRADICTION =
  'This proposal contradicts itself — re-reading the report cannot make it applicable; it has to be rejected or re-proposed.';
export const STRUCTURAL_SELF_DELETED = `An earlier change in this proposal deletes this block. ${SELF_CONTRADICTION}`;
export const STRUCTURAL_ANCHOR_SELF_DELETED = `An earlier change in this proposal deletes the block this one is anchored on. ${SELF_CONTRADICTION}`;
export const STRUCTURAL_TEMP_UNRESOLVED = `This is anchored on a block no earlier change in this proposal creates. ${SELF_CONTRADICTION}`;
export const STRUCTURAL_ANCHOR_SELF = `This block is anchored after itself. ${SELF_CONTRADICTION}`;
export const STRUCTURAL_NO_ANCHOR = `A new block has to say where it goes, and this one carries no anchor. ${SELF_CONTRADICTION}`;
export const STRUCTURAL_NO_IF_REV = `Replacing a block requires the revision it was written against, and this change carries none. ${SELF_CONTRADICTION}`;

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
   * Advisory only (§5.6). `true` = this op's anchors/revs no longer match
   * the simulated state — snapshot decay, a re-read could fix it.
   * `false` = they do (or the failure is structural, see `structural`),
   * `null` = we hold no block index, so there is nothing to compare and
   * no hint to show.
   */
  stale: boolean | null;
  /**
   * Set when the op is impossible against ANY version of the report
   * because the proposal contradicts itself — the kernel's `BadRequest`
   * class. Never staleness: no re-read can fix it (§5.2.1).
   */
  structural?: string;
  /**
   * Set on every op after the first one that cannot be applied. The batch
   * is atomic, so the kernel would never run these — showing a simulated
   * result for them would be fiction.
   */
  notSimulated?: boolean;
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

/** Recursively key-sorted copy — object key ORDER is not content. */
function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (isRecord(value)) {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value).sort()) out[key] = canonicalize(value[key]);
    return out;
  }
  return value;
}

/**
 * The kernel stores RENDERED content and compares canonical JSON, so
 * `{a:1,b:2}` and `{b:2,a:1}` are the same block and replacing one with
 * the other is an idempotent no-op (no rev bump). Comparing
 * `JSON.stringify` directly would bump the previewed rev on key order
 * alone and then flag the NEXT op's `if_rev` as out of date — a warning
 * about nothing.
 */
function sameContent(a: unknown, b: unknown): boolean {
  return JSON.stringify(canonicalize(a)) === JSON.stringify(canonicalize(b));
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

/** Why an op cannot be applied to the state the ops before it produced. */
type Failure =
  | { class: 'stale' }
  | { class: 'structural'; text: string };

/**
 * Walk `ops` in order against a simulation of `blocks`, returning one
 * before/after view per op.
 *
 * `blocks === undefined` means "the page has no block index" — every pane
 * comes back UNKNOWN, every `stale` comes back `null`, and NOTHING is
 * classified or halted: with no document to check preconditions against,
 * claiming an op fails would be an invention.
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
  /** Ids an EARLIER op of this proposal deleted (kernel: `deleted`). */
  const selfDeleted = new Set<string>();
  const views: OpView[] = [];
  /** Set once an op fails: the batch is atomic, so nothing after it runs. */
  let halted = false;

  const indexOf = (id: string) => doc.findIndex((b) => b.id === id);

  /**
   * Resolve an anchor the way `resolve_anchor_index` does: a `temp:` ref
   * no earlier op minted and a target this proposal already deleted are
   * STRUCTURAL; a target simply absent from the document is STALE.
   */
  const resolveAnchor = (
    anchor: ProposalAnchor,
  ): { index: number | null; failure?: Failure } => {
    if (anchor === 'at_start' || anchor === 'at_end') return { index: null };
    const raw = anchor.after_block_id;
    let target = raw;
    if (raw.startsWith(TEMP_REF_PREFIX)) {
      const mintedId = minted.get(raw.slice(TEMP_REF_PREFIX.length));
      if (mintedId == null) {
        return {
          index: null,
          failure: { class: 'structural', text: STRUCTURAL_TEMP_UNRESOLVED },
        };
      }
      target = mintedId;
    }
    if (selfDeleted.has(target)) {
      return {
        index: null,
        failure: {
          class: 'structural',
          text: STRUCTURAL_ANCHOR_SELF_DELETED,
        },
      };
    }
    const i = indexOf(target);
    if (i < 0) return { index: null, failure: { class: 'stale' } };
    return { index: i };
  };

  /** Before pane for an op whose target is missing from the simulation. */
  const missingPane = (failure?: Failure): PaneView => ({
    placeholder: !known
      ? UNKNOWN
      : failure?.class === 'structural'
        ? SELF_DELETED
        : GONE,
  });

  /** `known === false` means we have nothing to check against. */
  const classify = (failure: Failure | undefined) =>
    !known || failure == null
      ? { stale: known ? false : null, structural: undefined }
      : failure.class === 'stale'
        ? { stale: true, structural: undefined }
        : { stale: false, structural: failure.text };

  for (const op of ops) {
    const headline = opHeadline(op);

    // The kernel never reaches this op: an earlier one already failed and
    // the batch is all-or-nothing. Say that instead of simulating on.
    if (halted) {
      views.push({
        headline,
        stale: null,
        notSimulated: true,
        before: { placeholder: NOT_SIMULATED },
        after: { placeholder: NOT_SIMULATED },
      });
      continue;
    }

    // ---- creation (upsert with temp_id) --------------------------------
    if (op.op === 'upsert_block' && op.block_id == null) {
      const anchor = op.anchor;
      const resolved =
        anchor == null
          ? {
              index: null,
              failure: {
                class: 'structural' as const,
                text: STRUCTURAL_NO_ANCHOR,
              },
            }
          : resolveAnchor(anchor);
      const failure = known ? resolved.failure : undefined;
      const resolves = failure == null;
      let at = doc.length;
      if (anchor === 'at_start') at = 0;
      else if (anchor != null && anchor !== 'at_end') {
        const ti = resolved.index;
        at = ti != null && ti >= 0 ? ti + 1 : doc.length;
      }
      const id = `${TEMP_REF_PREFIX}${op.temp_id ?? 'new'}`;
      const payload = payloadOf(op);
      views.push({
        headline,
        ...classify(failure),
        before: { placeholder: 'No existing block — this one is new.' },
        after: {
          block: { id, rev: 1, kind: op.kind, payload },
          note: anchor ? `Insert ${anchorLabel(anchor)}` : undefined,
          position:
            known && resolves ? positionLine(at, doc.length + 1) : undefined,
        },
      });
      if (failure != null) {
        halted = true;
        continue;
      }
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
      let failure: Failure | undefined;
      if (selfDeleted.has(id)) {
        failure = { class: 'structural', text: STRUCTURAL_SELF_DELETED };
      } else if (live == null) {
        failure = { class: 'stale' };
      } else if (op.if_rev == null) {
        failure = { class: 'structural', text: STRUCTURAL_NO_IF_REV };
      } else if (live.rev !== op.if_rev) {
        failure = { class: 'stale' };
      }
      if (!known) failure = undefined;
      // Mirror `ReportDoc::upsert_block`: identical CONTENT is an
      // idempotent replace and does NOT bump the rev.
      const identical =
        live != null &&
        live.kind === op.kind &&
        sameContent(live.payload, payload);
      const rev =
        live == null
          ? (op.if_rev ?? 0)
          : identical
            ? live.rev
            : live.rev + 1;
      views.push({
        headline,
        ...classify(failure),
        before:
          live != null
            ? { block: asBlock(live), position: positionLine(i, doc.length) }
            : missingPane(failure),
        after: {
          block: { id, rev, kind: op.kind, payload },
          position: live != null ? positionLine(i, doc.length) : undefined,
        },
      });
      if (failure != null) {
        halted = true;
        continue;
      }
      if (live != null) doc[i] = { id, rev, kind: op.kind, payload };
      continue;
    }

    // ---- move ----------------------------------------------------------
    if (op.op === 'move_block') {
      const i = indexOf(op.block_id);
      const live = i >= 0 ? doc[i] : undefined;
      const anchorCheck = resolveAnchor(op.anchor);
      let failure: Failure | undefined;
      if (selfDeleted.has(op.block_id)) {
        failure = { class: 'structural', text: STRUCTURAL_SELF_DELETED };
      } else if (live == null) {
        failure = { class: 'stale' };
      } else if (anchorCheck.failure != null) {
        failure = anchorCheck.failure;
      } else if (anchorCheck.index === i) {
        failure = { class: 'structural', text: STRUCTURAL_ANCHOR_SELF };
      } else if (live.rev !== op.if_rev) {
        failure = { class: 'stale' };
      }
      if (!known) failure = undefined;
      const resolves = failure == null;
      let to = i;
      if (live != null && resolves) {
        if (op.anchor === 'at_start') to = 0;
        else if (op.anchor === 'at_end') to = doc.length - 1;
        else {
          const ti = anchorCheck.index ?? -1;
          // `move_block` removes the block first, so an anchor sitting
          // after it shifts left by one.
          to = ti > i ? ti : ti + 1;
        }
      }
      const moveNote = `Moved ${anchorLabel(op.anchor)}`;
      views.push({
        headline,
        ...classify(failure),
        before:
          live != null
            ? { block: asBlock(live), position: positionLine(i, doc.length) }
            : missingPane(failure),
        after:
          live != null
            ? {
                block: asBlock(live),
                note: moveNote,
                position: resolves ? positionLine(to, doc.length) : undefined,
              }
            : { ...missingPane(failure), note: moveNote },
      });
      if (failure != null) {
        halted = true;
        continue;
      }
      if (live != null && to !== i) {
        doc.splice(i, 1);
        doc.splice(to, 0, live);
      }
      continue;
    }

    // ---- delete ----------------------------------------------------------
    const i = indexOf(op.block_id);
    const live = i >= 0 ? doc[i] : undefined;
    let failure: Failure | undefined;
    if (selfDeleted.has(op.block_id)) {
      failure = { class: 'structural', text: STRUCTURAL_SELF_DELETED };
    } else if (live == null) {
      failure = { class: 'stale' };
    } else if (live.rev !== op.if_rev) {
      failure = { class: 'stale' };
    }
    if (!known) failure = undefined;
    views.push({
      headline,
      ...classify(failure),
      before:
        live != null
          ? { block: asBlock(live), position: positionLine(i, doc.length) }
          : missingPane(failure),
      after: { placeholder: 'Block removed.' },
    });
    if (failure != null) {
      halted = true;
      continue;
    }
    if (live != null) {
      doc.splice(i, 1);
      selfDeleted.add(op.block_id);
    }
  }

  return views;
}
