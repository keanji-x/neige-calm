// The ④ proposal channel's human side (issue #955 §5.5, PR-c).
//
// An app (plugin) cannot write the wave report; it *proposes* block edits
// and a human accepts or rejects them. This panel is that adjudication
// surface: it sits at the top of the report document — the thing the
// proposals are about — rather than introducing a new navigation concept,
// and it disappears entirely when nothing is pending.
//
// Two properties this UI must not get wrong:
//
//   * §5.5 — a proposal outlives its plugin. A stopped or uninstalled
//     plugin's pending proposal still renders (its raw `plugin_id` is what
//     we show) and stays adjudicable; nothing here consults plugin
//     liveness.
//   * §5.6 — pressing accept has four honest outcomes and each is
//     reported as itself: `accepted` (report changed), `stale` (the
//     document moved; recorded, report untouched), `400` (structurally
//     impossible; STAYS pending and can still be rejected), `409`
//     (already resolved elsewhere; the list refreshes).
//
// And one thing the PANES must not get wrong — see `blockPreview.tsx`:
// unadjudicated plugin content is never executed in the preview.

import { useEffect, useMemo, useRef } from 'react';
import { CalmApiError } from '../../api/calm';
import {
  useAcceptProposalMutation,
  useRejectProposalMutation,
  useWaveProposalsQuery,
} from '../../api/queries';
import type { PendingProposal } from '../../api/wire';
import type { ReportBlock } from '../../cards/builtins/wave-report';
import { useState } from '../../shared/state';
import { formatRelativeTime } from '../../shared/relativeTime';
import { ProposalOpDiff } from './opDiff';
import { simulateProposal } from './simulate';

export interface ProposalsPanelProps {
  waveId: string;
  /** The report card's current blocks — the "before" side of every diff,
   *  and the basis of the advisory staleness hint. `undefined` means the
   *  page holds NO block index (body-only report, no report card, or an
   *  unsupported payload version); the panes then render "unknown", never
   *  "gone". */
  blocks?: ReportBlock[];
}

type Tone = 'ok' | 'warn' | 'error';
/** `staysPending`: the kernel did NOT resolve the proposal, so its row
 *  keeps its Accept/Reject buttons and no verdict notice will ever mount
 *  for it — nothing to move focus to. */
type Feedback = { tone: Tone; text: string; staysPending?: boolean };
/** A verdict the user just produced. Held at panel level on purpose: an
 *  `accepted` / `stale` / `rejected` proposal leaves the pending list
 *  immediately, and if the message lived on the card it would vanish with
 *  it — taking the stale explanation ("your accept did nothing, and why")
 *  with it. */
type Outcome = Feedback & {
  proposalId: string;
  pluginId: string;
  focusWasInside: boolean;
};

function describeError(error: unknown): Feedback {
  if (error instanceof CalmApiError) {
    switch (error.status) {
      case 400:
        return {
          tone: 'error',
          staysPending: true,
          text: `This proposal cannot be applied to any version of the report, so it stays pending — you can still reject it. Kernel said: ${error.message}`,
        };
      case 409:
        return {
          tone: 'warn',
          text: 'This proposal was already resolved elsewhere. The list has been refreshed.',
        };
      case 404:
        return {
          tone: 'warn',
          text: 'This proposal (or its wave) no longer exists. The list has been refreshed.',
        };
      case 403:
        return {
          tone: 'error',
          staysPending: true,
          text: 'Only the signed-in user may adjudicate proposals.',
        };
      default:
        return { tone: 'error', text: error.message || `HTTP ${error.status}` };
    }
  }
  // No HTTP status at all: the request never produced an answer (network
  // drop, aborted fetch). The server may well have COMMITTED — claiming
  // "Request failed" next to a report that did change would be a lie.
  const detail = error instanceof Error && error.message ? ` (${error.message})` : '';
  return {
    tone: 'warn',
    text: `We could not confirm this decision${detail} — the request did not come back. The list has been refreshed; check the report to see whether it was applied.`,
  };
}

function ProposalCard({
  proposal,
  blocks,
  feedback,
  onOutcome,
}: {
  proposal: PendingProposal;
  blocks?: ReportBlock[];
  feedback: Feedback | null;
  onOutcome: (outcome: Feedback | null, focusWasInside: boolean) => void;
}) {
  const accept = useAcceptProposalMutation();
  const reject = useRejectProposalMutation();
  const setFeedback = onOutcome;
  // `isPending` only flips on the NEXT render, so two fast clicks both
  // pass the `busy` check and fire two POSTs — the second one's 409 then
  // overwrites the user's own success. A synchronous ref closes the gap.
  const inFlight = useRef(false);
  const cardRef = useRef<HTMLElement>(null);
  const busy = accept.isPending || reject.isPending;
  // Walking the ops (and canonicalizing every replace payload) is pure
  // work over two inputs — no reason to redo it on every keystroke-level
  // rerender of this card.
  const views = useMemo(
    () => simulateProposal(proposal.ops, blocks),
    [proposal.ops, blocks],
  );
  const vars = { id: proposal.proposal_id, waveId: proposal.wave_id };

  const onAccept = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    const focusWasInside =
      cardRef.current?.contains(document.activeElement) ?? false;
    setFeedback(null, focusWasInside);
    try {
      const res = await accept.mutateAsync(vars);
      if (res.decision === 'accepted') {
        setFeedback(
          {
            tone: 'ok',
            text: 'Accepted — the report has been updated.',
          },
          focusWasInside,
        );
      } else if (res.decision === 'stale') {
        // NOT an error: the kernel adjudicated, and the verdict was
        // "the document moved under this proposal" (§5.6).
        setFeedback(
          {
            tone: 'warn',
            text:
              'Went stale — the report moved after this proposal was written, so nothing was applied and the report is unchanged.' +
              (res.reason ? ` Kernel said: ${res.reason}` : ''),
          },
          focusWasInside,
        );
      } else {
        // `rejected` / `withdrawn` from an ACCEPT is not a shape §5.6
        // produces. Say what came back rather than narrating a report
        // change that did not happen.
        setFeedback(
          {
            tone: 'warn',
            text: `The kernel recorded this proposal as "${res.decision}", not accepted — the report was not changed by this action.`,
          },
          focusWasInside,
        );
      }
    } catch (error) {
      setFeedback(describeError(error), focusWasInside);
    } finally {
      inFlight.current = false;
    }
  };

  const onReject = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    const focusWasInside =
      cardRef.current?.contains(document.activeElement) ?? false;
    setFeedback(null, focusWasInside);
    try {
      await reject.mutateAsync(vars);
      setFeedback(
        {
          tone: 'ok',
          text: 'Rejected — the report is unchanged.',
        },
        focusWasInside,
      );
    } catch (error) {
      setFeedback(describeError(error), focusWasInside);
    } finally {
      inFlight.current = false;
    }
  };

  return (
    <article
      ref={cardRef}
      className="pp-card"
      aria-label={`Proposal ${proposal.proposal_id} from ${proposal.plugin_id}`}
    >
      <header className="pp-card-head">
        <div className="pp-card-meta">
          {/* §5.5: the raw plugin id, whether or not that plugin is still
              installed or running. */}
          <span className="pp-plugin">{proposal.plugin_id}</span>
          <span className="pp-dot" aria-hidden="true" />
          <span>{formatRelativeTime('Submitted', proposal.created_at)}</span>
          <span className="pp-dot" aria-hidden="true" />
          <span>
            {proposal.ops.length === 1
              ? '1 change'
              : `${proposal.ops.length} changes`}
          </span>
        </div>
        <div className="pp-card-actions">
          <button
            type="button"
            className="pp-btn pp-btn--accept"
            onClick={() => void onAccept()}
            disabled={busy}
          >
            {accept.isPending ? 'Accepting…' : 'Accept'}
          </button>
          <button
            type="button"
            className="pp-btn"
            onClick={() => void onReject()}
            disabled={busy}
          >
            {reject.isPending ? 'Rejecting…' : 'Reject'}
          </button>
        </div>
      </header>
      {proposal.note.trim().length > 0 && (
        <p className="pp-note">{proposal.note}</p>
      )}
      {feedback && (
        <p
          className={`pp-feedback pp-feedback--${feedback.tone}`}
          role={feedback.tone === 'ok' ? 'status' : 'alert'}
        >
          {feedback.text}
        </p>
      )}
      <ul className="pp-ops">
        {views.map((view, i) => (
          <ProposalOpDiff
            // Ops are an ordered list applied in sequence, and the same
            // block may legitimately appear twice, so position IS the
            // identity here.
            key={i}
            view={view}
          />
        ))}
      </ul>
    </article>
  );
}

export function ProposalsPanel({ waveId, blocks }: ProposalsPanelProps) {
  const query = useWaveProposalsQuery(waveId);
  const [outcomes, setOutcomes] = useState<Outcome[]>([]);
  // `WaveReportPage` handles a wave switch IN PLACE (no remount), so panel
  // state would otherwise survive into a wave the verdict has nothing to
  // do with — a wave-A verdict rendered above wave B's untouched report,
  // and a panel appearing on a wave with nothing pending.
  const [shownWaveId, setShownWaveId] = useState(waveId);
  if (shownWaveId !== waveId) {
    setShownWaveId(waveId);
    setOutcomes([]);
  }

  const noticeRefs = useRef(new Map<string, HTMLParagraphElement | null>());
  const proposals = query.data?.proposals ?? [];

  const recordOutcome = (
    proposal: PendingProposal,
    outcome: Feedback | null,
    focusWasInside: boolean,
  ) => {
    setOutcomes((current) => {
      const rest = current.filter(
        (o) => o.proposalId !== proposal.proposal_id,
      );
      return outcome
        ? [
            ...rest,
            {
              ...outcome,
              proposalId: proposal.proposal_id,
              pluginId: proposal.plugin_id,
              focusWasInside,
            },
          ]
        : rest;
    });
  };

  const pendingIds = new Set(proposals.map((p) => p.proposal_id));
  // A resolved proposal is gone from the list, but its verdict is the
  // whole point of having pressed the button — especially `stale`, whose
  // message ("nothing was applied, and here is why") has no other home.
  const notices = outcomes.filter((o) => !pendingIds.has(o.proposalId));
  const noticeKey = notices.map((n) => n.proposalId).join('|');

  // Observe immutable query transitions here, never from the async action's
  // render closure. A first-seen outcome is consumed while its row remains;
  // that prevents a failed request's notice from stealing focus if an
  // unrelated later refetch removes the proposal. Query data may update
  // just before the async continuation records the outcome, so remember a
  // departure until that first outcome render arrives.
  const previousPendingIds = useRef(pendingIds);
  const observedOutcomeIds = useRef(new Set<string>());
  const departuresAwaitingOutcome = useRef(new Set<string>());
  const previous = previousPendingIds.current;
  const departed = new Set(
    [...previous].filter((proposalId) => !pendingIds.has(proposalId)),
  );
  for (const proposalId of departed) {
    if (!outcomes.some((outcome) => outcome.proposalId === proposalId)) {
      departuresAwaitingOutcome.current.add(proposalId);
    }
  }
  let focusProposalId: string | null = null;
  for (const outcome of outcomes) {
    if (observedOutcomeIds.current.has(outcome.proposalId)) continue;
    observedOutcomeIds.current.add(outcome.proposalId);
    if (
      outcome.focusWasInside &&
      !pendingIds.has(outcome.proposalId) &&
      (departed.has(outcome.proposalId) ||
        departuresAwaitingOutcome.current.has(outcome.proposalId))
    ) {
      focusProposalId = outcome.proposalId;
    }
    departuresAwaitingOutcome.current.delete(outcome.proposalId);
  }
  for (const proposalId of [...observedOutcomeIds.current]) {
    if (!outcomes.some((outcome) => outcome.proposalId === proposalId)) {
      observedOutcomeIds.current.delete(proposalId);
    }
  }
  previousPendingIds.current = pendingIds;

  // The refetch unmounts the Accept/Reject button the user was standing
  // on. Move focus to the verdict that replaced it, or keyboard users are
  // stranded at document root while the answer renders elsewhere.
  useEffect(() => {
    if (focusProposalId == null) return;
    const el = noticeRefs.current.get(focusProposalId);
    el?.focus();
  }, [focusProposalId, noticeKey]);

  // A transient list error must NOT swallow the verdict the user just
  // produced — for a `stale` accept this panel is the only place that
  // explanation exists.
  const listError = query.error;
  if (proposals.length === 0 && notices.length === 0 && listError == null) {
    // Empty state is *no* state: an adjudication surface with nothing to
    // adjudicate is noise on every report the channel never touches.
    return null;
  }

  return (
    <section className="pp-panel" aria-label="App proposals awaiting review">
      <h2 className="pp-panel-head">
        App proposals
        {/* No badge when the only thing left is a verdict notice — a
            header reading "App proposals 0" is worse than no count. */}
        {proposals.length > 0 && (
          <span className="pp-count">{proposals.length}</span>
        )}
      </h2>
      <p className="pp-panel-hint">
        These are proposed by apps and change nothing until you accept them.
      </p>
      {listError != null && (
        <p className="pp-feedback pp-feedback--error" role="alert">
          Could not load app proposals: {listError.message}
        </p>
      )}
      {notices.map((notice) => (
        <p
          key={notice.proposalId}
          ref={(el) => {
            noticeRefs.current.set(notice.proposalId, el);
          }}
          tabIndex={-1}
          className={`pp-feedback pp-feedback--${notice.tone}`}
          role={notice.tone === 'ok' ? 'status' : 'alert'}
        >
          <span className="pp-plugin">{notice.pluginId}</span> {notice.text}
          <button
            type="button"
            className="pp-dismiss"
            aria-label={`Dismiss verdict for proposal ${notice.proposalId} from ${notice.pluginId}`}
            onClick={() =>
              setOutcomes((current) =>
                current.filter((o) => o.proposalId !== notice.proposalId),
              )
            }
          >
            Dismiss
          </button>
        </p>
      ))}
      {proposals.map((proposal) => (
        <ProposalCard
          key={proposal.proposal_id}
          proposal={proposal}
          blocks={blocks}
          feedback={
            outcomes.find((o) => o.proposalId === proposal.proposal_id) ?? null
          }
          onOutcome={(outcome, focusWasInside) =>
            recordOutcome(proposal, outcome, focusWasInside)
          }
        />
      ))}
    </section>
  );
}
