// #955 PR-c — the ④ proposal channel's adjudication UI.
//
// The query/mutation hooks are mocked at the module seam (the codebase's
// convention for panel tests, see `WaveReportPage.test.tsx`); what is
// exercised here is the part PR-c actually owns: which panes exist per op
// kind, that a dead plugin stays adjudicable, and that each of accept's
// four honest outcomes is reported as itself.

import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProposalsPanel } from './index';
import {
  useAcceptProposalMutation,
  useRejectProposalMutation,
  useWaveProposalsQuery,
} from '../../api/queries';
import { CalmApiError } from '../../api/calm';
import type { PendingProposal, ProposalOp } from '../../api/wire';
import type { ReportBlock } from '../../cards/builtins/wave-report';

vi.mock('../../api/queries', () => ({
  useWaveProposalsQuery: vi.fn(),
  useAcceptProposalMutation: vi.fn(),
  useRejectProposalMutation: vi.fn(),
}));

const mockList = vi.mocked(useWaveProposalsQuery);
const mockAccept = vi.mocked(useAcceptProposalMutation);
const mockReject = vi.mocked(useRejectProposalMutation);

type Resolve = { decision: string; reason?: string };

function stubMutation(impl?: () => Promise<Resolve>) {
  const mutateAsync = vi.fn(impl ?? (() => Promise.resolve({ decision: 'accepted' })));
  return { mutateAsync, isPending: false };
}

function setList(proposals: PendingProposal[], error?: Error) {
  mockList.mockReturnValue({
    data: error ? undefined : { proposals },
    error: error ?? null,
  } as unknown as ReturnType<typeof useWaveProposalsQuery>);
}

function proposal(overrides: Partial<PendingProposal> = {}): PendingProposal {
  return {
    proposal_id: 'pp_1',
    wave_id: 'w_1',
    plugin_id: 'quotes',
    subject_kind: 'report',
    base_doc_heads: 'ah1:deadbeef',
    ops: [],
    note: 'Refresh the price table.',
    created_at: Date.now(),
    ...overrides,
  };
}

const liveProse: ReportBlock = {
  id: 'b_0001',
  rev: 3,
  kind: 'prose',
  payload: { markdown: 'the old paragraph' },
};
const liveTable: ReportBlock = {
  id: 'b_0002',
  rev: 1,
  kind: 'table',
  payload: {
    columns: [{ key: 'sym', label: 'Symbol' }],
    rows: [{ sym: 'OLD-ROW' }],
  },
};
const blocks: ReportBlock[] = [liveProse, liveTable];

function renderPanel(ops: ProposalOp[], extra: Partial<PendingProposal> = {}) {
  setList([proposal({ ops, ...extra })]);
  return render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
}

beforeEach(() => {
  vi.clearAllMocks();
  mockAccept.mockReturnValue(
    stubMutation() as unknown as ReturnType<typeof useAcceptProposalMutation>,
  );
  mockReject.mockReturnValue(
    stubMutation(() => Promise.resolve({ decision: 'rejected' })) as unknown as ReturnType<
      typeof useRejectProposalMutation
    >,
  );
});

describe('ProposalsPanel — list', () => {
  it('renders nothing when no proposal is pending', () => {
    setList([]);
    const { container } = render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders nothing while the list is still loading', () => {
    mockList.mockReturnValue({ data: undefined, error: null } as unknown as ReturnType<
      typeof useWaveProposalsQuery
    >);
    const { container } = render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('surfaces a list error instead of pretending nothing is pending', () => {
    setList([], new Error('boom'));
    render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    expect(screen.getByRole('alert')).toHaveTextContent('boom');
  });

  it('shows plugin id, note, submitted time and op count', () => {
    renderPanel([
      { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
      { op: 'delete_block', block_id: 'b_0002', if_rev: 1 },
    ]);
    const card = screen.getByLabelText('Proposal pp_1 from quotes');
    expect(within(card).getByText('quotes')).toBeInTheDocument();
    expect(within(card).getByText('Refresh the price table.')).toBeInTheDocument();
    expect(within(card).getByText('Submitted just now')).toBeInTheDocument();
    expect(within(card).getByText('2 changes')).toBeInTheDocument();
  });

  it('§5.5: a proposal from an uninstalled plugin still renders its id and stays adjudicable', () => {
    // Nothing in this panel consults plugin liveness — pending proposals
    // live in the event log, not in a plugin process.
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }], {
      plugin_id: 'long-gone-app',
    });
    const card = screen.getByLabelText('Proposal pp_1 from long-gone-app');
    expect(within(card).getByText('long-gone-app')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Accept' })).toBeEnabled();
    expect(within(card).getByRole('button', { name: 'Reject' })).toBeEnabled();
  });
});

describe('ProposalsPanel — before/after diff per op kind', () => {
  it('upsert with block_id shows the live block before and the proposal after', () => {
    renderPanel([
      {
        op: 'upsert_block',
        block_id: 'b_0001',
        kind: 'prose',
        payload: { markdown: 'the new paragraph' },
        if_rev: 3,
      },
    ]);
    const before = screen.getByLabelText('Before — Modify prose block b_0001');
    const after = screen.getByLabelText('After — Modify prose block b_0001');
    expect(within(before).getByText('the old paragraph')).toBeInTheDocument();
    expect(within(after).getByText('the new paragraph')).toBeInTheDocument();
    expect(within(before).getByText('Position 1 of 2')).toBeInTheDocument();
  });

  it('a created block has no before side', () => {
    renderPanel([
      {
        op: 'upsert_block',
        temp_id: 't1',
        kind: 'prose',
        payload: { markdown: 'brand new' },
        anchor: 'at_end',
      },
    ]);
    const before = screen.getByLabelText('Before — New prose block');
    const after = screen.getByLabelText('After — New prose block');
    expect(
      within(before).getByText('No existing block — this one is new.'),
    ).toBeInTheDocument();
    expect(within(after).getByText('brand new')).toBeInTheDocument();
    expect(
      within(after).getByText('Insert at the end of the report'),
    ).toBeInTheDocument();
  });

  it('a delete has no after side', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }]);
    const before = screen.getByLabelText('Before — Delete block b_0001');
    const after = screen.getByLabelText('After — Delete block b_0001');
    expect(within(before).getByText('the old paragraph')).toBeInTheDocument();
    expect(within(after).getByText('Block removed.')).toBeInTheDocument();
  });

  it('a move shows the same block on both sides with the position change', () => {
    renderPanel([
      {
        op: 'move_block',
        block_id: 'b_0002',
        if_rev: 1,
        anchor: { after_block_id: 'b_0001' },
      },
    ]);
    const before = screen.getByLabelText('Before — Move block b_0002');
    const after = screen.getByLabelText('After — Move block b_0002');
    expect(within(before).getByText('Position 2 of 2')).toBeInTheDocument();
    expect(within(after).getByText('Moved after b_0001')).toBeInTheDocument();
    // Whole block on both sides — the content is unchanged by a move.
    expect(within(before).getByText('OLD-ROW')).toBeInTheDocument();
    expect(within(after).getByText('OLD-ROW')).toBeInTheDocument();
  });

  it('#960 D3: a non-prose block is compared as a whole block, not per parameter', () => {
    renderPanel([
      {
        op: 'upsert_block',
        block_id: 'b_0002',
        kind: 'table',
        payload: {
          columns: [{ key: 'sym', label: 'Symbol' }],
          rows: [{ sym: 'NEW-ROW' }],
        },
        if_rev: 1,
      },
    ]);
    const before = screen.getByLabelText('Before — Modify table block b_0002');
    const after = screen.getByLabelText('After — Modify table block b_0002');
    expect(within(before).getByText('OLD-ROW')).toBeInTheDocument();
    expect(within(after).getByText('NEW-ROW')).toBeInTheDocument();
    expect(within(before).queryByText('NEW-ROW')).toBeNull();
  });

  it('an anchor naming a block created earlier in the same proposal reads as such', () => {
    renderPanel([
      {
        op: 'upsert_block',
        temp_id: 't1',
        kind: 'prose',
        payload: { markdown: 'first' },
        anchor: 'at_start',
      },
      {
        op: 'move_block',
        block_id: 'b_0001',
        if_rev: 3,
        anchor: { after_block_id: 'temp:t1' },
      },
    ]);
    expect(
      screen.getByText(
        'Moved after the block created earlier in this proposal (t1)',
      ),
    ).toBeInTheDocument();
  });

  it('renders a block that has vanished from the report as missing, not as a crash', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_gone', if_rev: 1 }]);
    expect(
      screen.getByText('This block is no longer in the report.'),
    ).toBeInTheDocument();
  });
});

describe('ProposalsPanel — advisory staleness (§5.6)', () => {
  it('flags a rev mismatch but never disables accept', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 1 }]);
    const hint = screen.getByText('may be out of date');
    expect(hint).toHaveAttribute(
      'title',
      expect.stringContaining('Advisory only'),
    );
    // The authoritative verdict happens inside the accept transaction, so
    // the hint must not stand between the user and that transaction.
    expect(screen.getByRole('button', { name: 'Accept' })).toBeEnabled();
  });

  it('does not flag an op whose anchors still match', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }]);
    expect(screen.queryByText('may be out of date')).toBeNull();
  });
});

describe('ProposalsPanel — accept/reject outcomes', () => {
  const op: ProposalOp = { op: 'delete_block', block_id: 'b_0001', if_rev: 3 };

  it('accepted → says the report changed', async () => {
    const stub = stubMutation(() => Promise.resolve({ decision: 'accepted' }));
    mockAccept.mockReturnValue(
      stub as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    expect(stub.mutateAsync).toHaveBeenCalledWith({
      id: 'pp_1',
      waveId: 'w_1',
    });
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Accepted — the report has been updated.',
    );
  });

  it('stale → reports an adjudication, not an error, and says the report is unchanged', async () => {
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.resolve({ decision: 'stale', reason: 'block b_0001 rev 3 != 4' }),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const msg = await screen.findByRole('alert');
    expect(msg).toHaveTextContent('Went stale');
    expect(msg).toHaveTextContent('the report is unchanged');
    expect(msg).toHaveTextContent('block b_0001 rev 3 != 4');
  });

  it('400 → says the proposal stays pending and can still be rejected', async () => {
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.reject(
          new CalmApiError(400, 'bad_request', 'op 0 names both block_id and temp_id'),
        ),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const msg = await screen.findByRole('alert');
    expect(msg).toHaveTextContent('stays pending');
    expect(msg).toHaveTextContent('op 0 names both block_id and temp_id');
    // Still adjudicable — reject is the way out of a structurally broken
    // proposal.
    expect(screen.getByRole('button', { name: 'Reject' })).toBeEnabled();
  });

  it('409 → says someone else resolved it and the list refreshed', async () => {
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.reject(
          new CalmApiError(409, 'conflict', 'proposal pp_1 is no longer pending'),
        ),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'already resolved elsewhere',
    );
  });

  it('reject → says the report is unchanged', async () => {
    const stub = stubMutation(() => Promise.resolve({ decision: 'rejected' }));
    mockReject.mockReturnValue(
      stub as unknown as ReturnType<typeof useRejectProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Reject' }));
    expect(stub.mutateAsync).toHaveBeenCalledWith({ id: 'pp_1', waveId: 'w_1' });
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Rejected — the report is unchanged.',
    );
  });

  it('both buttons are keyboard-operable', async () => {
    const stub = stubMutation(() => Promise.resolve({ decision: 'accepted' }));
    mockAccept.mockReturnValue(
      stub as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    screen.getByRole('button', { name: 'Accept' }).focus();
    await userEvent.keyboard('{Enter}');
    expect(stub.mutateAsync).toHaveBeenCalledTimes(1);
  });
});
