// #955 PR-c — the ④ proposal channel's adjudication UI.
//
// The query/mutation hooks are mocked at the module seam (the codebase's
// convention for panel tests, see `WaveReportPage.test.tsx`); what is
// exercised here is the part PR-c actually owns: which panes exist per op
// kind, that the panes follow §5.2.1's SEQUENTIAL semantics, that an
// unadjudicated `app` block is never live-mounted, that a dead plugin
// stays adjudicable, and that each of accept's four honest outcomes is
// reported as itself.
//
// The `onSettled` invalidation wiring those mocks hide is covered for
// real in `api/queries.test.tsx`; the two `invalidationPolicies` entries
// in `app/eventBridge.test.tsx`.

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

  it('an empty ops array renders an adjudicable card with no panes', () => {
    // The kernel refuses an empty proposal at submit, but a client must
    // not crash (or claim changes) if one ever reaches it.
    renderPanel([]);
    const card = screen.getByLabelText('Proposal pp_1 from quotes');
    expect(within(card).getByText('0 changes')).toBeInTheDocument();
    expect(within(card).queryByLabelText(/^Before —/)).toBeNull();
    expect(within(card).getByRole('button', { name: 'Accept' })).toBeEnabled();
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

  it('the header count is dropped rather than reading "0" when only a notice remains', async () => {
    const list: PendingProposal[] = [
      proposal({ ops: [{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }] }),
    ];
    mockList.mockImplementation(
      () =>
        ({ data: { proposals: list }, error: null }) as unknown as ReturnType<
          typeof useWaveProposalsQuery
        >,
    );
    mockAccept.mockReturnValue(
      stubMutation(() => {
        list.length = 0;
        return Promise.resolve({ decision: 'accepted' });
      }) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    const { container } = render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    expect(container.querySelector('.pp-count')).toHaveTextContent('1');
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    await screen.findByRole('status');
    expect(container.querySelector('.pp-count')).toBeNull();
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
    expect(within(after).getByText('Position 3 of 3')).toBeInTheDocument();
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
        anchor: 'at_start',
      },
    ]);
    const before = screen.getByLabelText('Before — Move block b_0002');
    const after = screen.getByLabelText('After — Move block b_0002');
    expect(within(before).getByText('Position 2 of 2')).toBeInTheDocument();
    expect(
      within(after).getByText('Moved at the top of the report'),
    ).toBeInTheDocument();
    expect(within(after).getByText('Position 1 of 2')).toBeInTheDocument();
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

describe('ProposalsPanel — §5.2.1 sequential preview', () => {
  it('a later op sees the payload an earlier op in the same proposal wrote', () => {
    // upsert(b_0001) then move(b_0001): the move must preview the NEW
    // paragraph, because that is the document the move will act on.
    renderPanel([
      {
        op: 'upsert_block',
        block_id: 'b_0001',
        kind: 'prose',
        payload: { markdown: 'the new paragraph' },
        if_rev: 3,
      },
      {
        op: 'move_block',
        block_id: 'b_0001',
        if_rev: 4,
        anchor: 'at_end',
      },
    ]);
    const moveBefore = screen.getByLabelText('Before — Move block b_0001');
    expect(within(moveBefore).getByText('the new paragraph')).toBeInTheDocument();
    expect(within(moveBefore).queryByText('the old paragraph')).toBeNull();
  });

  it('an op anchored on a block an earlier op deleted is STRUCTURAL, not staleness', () => {
    renderPanel([
      { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
      {
        op: 'move_block',
        block_id: 'b_0002',
        if_rev: 1,
        anchor: { after_block_id: 'b_0001' },
      },
    ]);
    // The delete itself is clean; the move against the post-delete
    // document is the one that cannot anchor — and the kernel calls that
    // `BadRequest`, not stale (`ensure_not_self_deleted`). "May be out of
    // date" would promise a re-read fixes it; nothing fixes it.
    expect(screen.queryByText('may be out of date')).toBeNull();
    const moveHead = screen.getByLabelText('Before — Move block b_0002')
      .parentElement!.parentElement!;
    expect(within(moveHead).getByText('cannot be applied')).toBeInTheDocument();
    expect(
      within(moveHead).getByText(
        /An earlier change in this proposal deletes the block this one is anchored on\./,
      ),
    ).toHaveTextContent('re-reading the report cannot make it applicable');
  });

  it('a temp-id creation is a real block for the ops that follow it', () => {
    renderPanel([
      {
        op: 'upsert_block',
        temp_id: 't1',
        kind: 'prose',
        payload: { markdown: 'brand new' },
        anchor: 'at_start',
      },
      {
        op: 'move_block',
        block_id: 'b_0002',
        if_rev: 1,
        anchor: { after_block_id: 'temp:t1' },
      },
    ]);
    // The created block is at position 1 of 3, so the move lands at 2 —
    // and nothing is flagged stale, because the anchor resolves.
    expect(screen.queryByText('may be out of date')).toBeNull();
    const after = screen.getByLabelText('After — Move block b_0002');
    expect(within(after).getByText('Position 2 of 3')).toBeInTheDocument();
  });

  it('an earlier move changes where a later op sits', () => {
    renderPanel([
      { op: 'move_block', block_id: 'b_0002', if_rev: 1, anchor: 'at_start' },
      { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
    ]);
    // b_0001 started at position 1; after b_0002 moved to the top it is 2.
    const before = screen.getByLabelText('Before — Delete block b_0001');
    expect(within(before).getByText('Position 2 of 2')).toBeInTheDocument();
  });
});

describe('ProposalsPanel — the simulation matches the kernel batch', () => {
  it('a replace that only reorders keys is idempotent, so the next op is not flagged', () => {
    // The kernel compares CANONICAL content: `{columns,rows}` and
    // `{rows,columns}` are the same block, so the rev does not move and
    // the following op's `if_rev: 1` still matches. A key-order-sensitive
    // comparison here would bump the rev and warn about nothing.
    renderPanel([
      {
        op: 'upsert_block',
        block_id: 'b_0002',
        kind: 'table',
        payload: {
          rows: [{ sym: 'OLD-ROW' }],
          columns: [{ label: 'Symbol', key: 'sym' }],
        },
        if_rev: 1,
      },
      { op: 'delete_block', block_id: 'b_0002', if_rev: 1 },
    ]);
    expect(screen.queryByText('may be out of date')).toBeNull();
    expect(screen.queryByText('cannot be applied')).toBeNull();
    expect(screen.queryByText('not previewed')).toBeNull();
  });

  it('stops at the first op the kernel would reject instead of simulating on', () => {
    // The batch is atomic: a rev-mismatched replace aborts the whole
    // proposal, so the ops after it never run and must not be shown as a
    // state the report could reach.
    renderPanel([
      {
        op: 'upsert_block',
        block_id: 'b_0001',
        kind: 'prose',
        payload: { markdown: 'the new paragraph' },
        if_rev: 99,
      },
      { op: 'delete_block', block_id: 'b_0002', if_rev: 1 },
    ]);
    // The failing op keeps its own honest hint…
    expect(screen.getByText('may be out of date')).toBeInTheDocument();
    // …and the one after it is explicitly NOT simulated.
    expect(screen.getByText('not previewed')).toBeInTheDocument();
    const after = screen.getByLabelText('After — Delete block b_0002');
    expect(within(after).getByText('Not previewed.')).toBeInTheDocument();
    expect(within(after).queryByText('Block removed.')).toBeNull();
    const before = screen.getByLabelText('Before — Delete block b_0002');
    expect(within(before).queryByText('OLD-ROW')).toBeNull();
  });

  it('a creation anchored on a temp id nothing creates is structural, and halts', () => {
    renderPanel([
      {
        op: 'upsert_block',
        temp_id: 't2',
        kind: 'prose',
        payload: { markdown: 'brand new' },
        anchor: { after_block_id: 'temp:never-made' },
      },
      { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
    ]);
    expect(screen.queryByText('may be out of date')).toBeNull();
    expect(screen.getByText('cannot be applied')).toBeInTheDocument();
    expect(
      screen.getByText(
        /anchored on a block no earlier change in this proposal creates/,
      ),
    ).toHaveTextContent('re-reading the report cannot make it applicable');
    // The unresolved creation is NOT minted into the simulated document…
    const after = screen.getByLabelText('After — New prose block');
    expect(within(after).queryByText(/^Position /)).toBeNull();
    // …and the op behind it is not simulated.
    expect(screen.getByText('not previewed')).toBeInTheDocument();
  });

  it('replacing a block this same proposal already deleted reads as self-contradiction', () => {
    renderPanel([
      { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
      {
        op: 'upsert_block',
        block_id: 'b_0001',
        kind: 'prose',
        payload: { markdown: 'resurrected' },
        if_rev: 3,
      },
    ]);
    expect(screen.queryByText('may be out of date')).toBeNull();
    expect(screen.getByText('cannot be applied')).toBeInTheDocument();
    const before = screen.getByLabelText('Before — Modify prose block b_0001');
    expect(
      within(before).getByText(
        'An earlier change in this proposal deletes this block.',
      ),
    ).toBeInTheDocument();
    // Not the retryable narration.
    expect(
      screen.queryByText('This block is no longer in the report.'),
    ).toBeNull();
  });
});

describe('ProposalsPanel — unadjudicated plugin code is never executed (§5.4/§5.6)', () => {
  const appBlock: ReportBlock = {
    id: 'b_app',
    rev: 2,
    kind: 'app',
    payload: { src: '/api/plugins/quotes/resources/panel', title: 'Ticker' },
  };

  it('a proposed app block renders a static descriptor, not an iframe', () => {
    setList([
      proposal({
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_app',
            kind: 'app',
            payload: { src: '/api/plugins/quotes/resources/evil', height: 400 },
            if_rev: 2,
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={[appBlock]} />,
    );
    // BOTH panes: the "before" side would live-mount the EXISTING app
    // block, which is just as much plugin-controlled same-origin HTML.
    expect(container.querySelectorAll('iframe')).toHaveLength(0);
    const before = screen.getByLabelText('Before — Modify app block b_app');
    const after = screen.getByLabelText('After — Modify app block b_app');
    expect(
      within(before).getByText('/api/plugins/quotes/resources/panel'),
    ).toBeInTheDocument();
    expect(
      within(after).getByText('/api/plugins/quotes/resources/evil'),
    ).toBeInTheDocument();
    expect(
      within(after).getByText(
        'The embedded app runs only after you accept this proposal.',
      ),
    ).toBeInTheDocument();
  });

  it('a proposed delete of an app block does not mount it either', () => {
    setList([
      proposal({
        ops: [{ op: 'delete_block', block_id: 'b_app', if_rev: 2 }],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={[appBlock]} />,
    );
    expect(container.querySelectorAll('iframe')).toHaveLength(0);
    expect(screen.getByText('app block — not run in this preview')).toBeInTheDocument();
  });

  it('a proposed move of an app block does not mount it in either pane', () => {
    setList([
      proposal({
        ops: [
          {
            op: 'move_block',
            block_id: 'b_app',
            if_rev: 2,
            anchor: 'at_start',
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={[appBlock, liveProse]} />,
    );
    expect(container.querySelectorAll('iframe')).toHaveLength(0);
    expect(
      screen.getAllByText('app block — not run in this preview'),
    ).toHaveLength(2);
  });
});

describe('ProposalsPanel — the preview renderer is default-deny (§5.4/§5.6)', () => {
  it('an unknown / future block kind falls back to the static descriptor', () => {
    // The gate is an ALLOWLIST: a kind nobody has audited (a future
    // `video`/`embed`, or a kind a plugin simply invented) must not reach
    // a live renderer just because it is not called "app".
    const futureBlock: ReportBlock = {
      id: 'b_vid',
      rev: 1,
      kind: 'video',
      payload: { src: 'https://evil.example/track.mp4', title: 'Clip' },
    };
    setList([
      proposal({
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_vid',
            kind: 'video',
            payload: { src: 'https://evil.example/next.mp4' },
            if_rev: 1,
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={[futureBlock]} />,
    );
    expect(
      container.querySelectorAll('iframe, video, embed, object, img'),
    ).toHaveLength(0);
    expect(
      screen.getAllByText('video block — not run in this preview'),
    ).toHaveLength(2);
    // Not the "unsupported block kind" placeholder of the live renderer:
    // this is the deliberate inert descriptor, showing the URL as text.
    expect(
      screen.getByText('https://evil.example/next.mp4'),
    ).toBeInTheDocument();
  });

  it('a proposed image does not make the browser fetch a plugin-chosen URL', () => {
    // Zero-click view beacon: `![](…)` would fetch on mere DISPLAY of a
    // pending proposal, leaking "the adjudicator looked at proposal X"
    // plus IP/UA/Referer before any accept.
    setList([
      proposal({
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_0001',
            kind: 'prose',
            payload: {
              markdown: '![a shot](https://evil.example/px.png?who=me)',
            },
            if_rev: 3,
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={blocks} />,
    );
    expect(
      container.querySelectorAll('img, iframe, video, embed, object'),
    ).toHaveLength(0);
    const after = screen.getByLabelText('After — Modify prose block b_0001');
    expect(
      within(after).getByText('https://evil.example/px.png?who=me'),
    ).toBeInTheDocument();
    expect(within(after).getByText(/image not loaded in this preview/)).toBeVisible();
  });

  it('a proposed link is text carrying its destination, not live navigation', () => {
    setList([
      proposal({
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_0001',
            kind: 'prose',
            payload: { markdown: '[totally safe](https://evil.example/go)' },
            if_rev: 3,
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={blocks} />,
    );
    expect(container.querySelectorAll('a')).toHaveLength(0);
    const after = screen.getByLabelText('After — Modify prose block b_0001');
    // The destination is READABLE — that is the point of adjudication.
    expect(within(after).getByText('totally safe')).toBeInTheDocument();
    expect(
      within(after).getByText('https://evil.example/go'),
    ).toBeInTheDocument();
  });

  it('raw HTML in proposed prose stays escaped (regression pin)', () => {
    setList([
      proposal({
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_0001',
            kind: 'prose',
            payload: {
              markdown:
                '<img src=x onerror="alert(1)"><script>alert(2)</script>',
            },
            if_rev: 3,
          },
        ],
      }),
    ]);
    const { container } = render(
      <ProposalsPanel waveId="w_1" blocks={blocks} />,
    );
    expect(container.querySelectorAll('img, script')).toHaveLength(0);
  });
});

describe('ProposalsPanel — advisory staleness (§5.6)', () => {
  it('flags a rev mismatch but never disables accept, and says so in visible text', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 1 }]);
    expect(screen.getByText('may be out of date')).toBeInTheDocument();
    // Not a `title` tooltip: keyboard and touch users must be able to
    // read the qualifier that makes the hint non-authoritative.
    expect(
      screen.getByText(/Advisory only — the report changed since this proposal/),
    ).toBeVisible();
    // The authoritative verdict happens inside the accept transaction, so
    // the hint must not stand between the user and that transaction.
    expect(screen.getByRole('button', { name: 'Accept' })).toBeEnabled();
  });

  it('does not flag an op whose anchors still match', () => {
    renderPanel([{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }]);
    expect(screen.queryByText('may be out of date')).toBeNull();
  });

  it('with no block index at all, unknown renders as unknown — not as gone or stale', () => {
    // Reachable on a body-only report, a report with no card, and an
    // `unsupportedVersion` payload: `blocks` is simply undefined.
    setList([
      proposal({
        ops: [
          { op: 'delete_block', block_id: 'b_0001', if_rev: 3 },
          {
            op: 'move_block',
            block_id: 'b_0002',
            if_rev: 1,
            anchor: { after_block_id: 'b_0001' },
          },
        ],
      }),
    ]);
    render(<ProposalsPanel waveId="w_1" />);
    expect(screen.queryByText('This block is no longer in the report.')).toBeNull();
    expect(screen.queryByText('may be out of date')).toBeNull();
    expect(
      screen.getAllByText(
        'The report blocks are not loaded here, so this block cannot be shown.',
      ).length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: 'Accept' })).toBeEnabled();
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

  it('an unexpected decision is reported as itself, not as "accepted"', async () => {
    // `ProposalDecision` has four variants; a `rejected` / `withdrawn`
    // body coming back from ACCEPT must not be narrated as a report
    // change that did not happen.
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.resolve({ decision: 'withdrawn' }),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const msg = await screen.findByRole('alert');
    expect(msg).toHaveTextContent('"withdrawn"');
    expect(msg).toHaveTextContent('the report was not changed by this action');
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

  it('a 400 leaves no pending focus intent for a later refetch to cash in', async () => {
    // The row stays pending after a 400, so no verdict notice mounts and
    // the focus intent would survive indefinitely; whenever a background
    // refetch finally drops the row, focus would jump to the notice while
    // the user is somewhere else entirely.
    const list: PendingProposal[] = [proposal({ ops: [op] })];
    mockList.mockImplementation(
      () =>
        ({ data: { proposals: list }, error: null }) as unknown as ReturnType<
          typeof useWaveProposalsQuery
        >,
    );
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.reject(new CalmApiError(400, 'bad_request', 'contradicts itself')),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    const { rerender } = render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('stays pending');
    expect(screen.getByRole('button', { name: 'Accept' })).toBeInTheDocument();

    // Later: the proposal is withdrawn elsewhere and the list refetches.
    list.length = 0;
    rerender(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    const notice = await screen.findByRole('alert');
    expect(notice).toHaveTextContent('stays pending');
    expect(notice).not.toHaveFocus();
    expect(document.body).toHaveFocus();
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

  it('a network failure says we could not CONFIRM — the server may have committed', async () => {
    mockAccept.mockReturnValue(
      stubMutation(() =>
        Promise.reject(new TypeError('Failed to fetch')),
      ) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const msg = await screen.findByRole('alert');
    expect(msg).toHaveTextContent('could not confirm this decision');
    expect(msg).toHaveTextContent('Failed to fetch');
    expect(msg).toHaveTextContent('check the report');
    expect(msg).not.toHaveTextContent('Request failed');
  });

  it('two fast clicks fire exactly one POST', async () => {
    // `isPending` only flips on the next render, so the `disabled` prop
    // cannot be the guard here.
    let resolveIt: (r: Resolve) => void = () => {};
    const stub = stubMutation(
      () => new Promise<Resolve>((resolve) => (resolveIt = resolve)),
    );
    mockAccept.mockReturnValue(
      stub as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    renderPanel([op]);
    const btn = screen.getByRole('button', { name: 'Accept' });
    await userEvent.click(btn);
    await userEvent.click(btn);
    expect(stub.mutateAsync).toHaveBeenCalledTimes(1);
    resolveIt({ decision: 'accepted' });
    expect(await screen.findByRole('status')).toHaveTextContent('Accepted');
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

  it('keeps the stale explanation visible after the row leaves the pending list, and takes focus', async () => {
    // The resolved proposal disappears from the list on the next fetch —
    // the verdict must not disappear with it, and the keyboard user
    // standing on the now-unmounted Accept button must land on it.
    const list: PendingProposal[] = [proposal({ ops: [op] })];
    mockList.mockImplementation(
      () =>
        ({ data: { proposals: list }, error: null }) as unknown as ReturnType<
          typeof useWaveProposalsQuery
        >,
    );
    mockAccept.mockReturnValue(
      stubMutation(() => {
        list.length = 0;
        return Promise.resolve({ decision: 'stale', reason: 'heads moved' });
      }) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const msg = await screen.findByRole('alert');
    expect(msg).toHaveTextContent('Went stale');
    expect(msg).toHaveTextContent('heads moved');
    expect(screen.queryByRole('button', { name: 'Accept' })).toBeNull();
    expect(msg).toHaveFocus();

    await userEvent.click(
      screen.getByRole('button', {
        name: 'Dismiss verdict for proposal pp_1 from quotes',
      }),
    );
    // Nothing pending and nothing to report → the panel is gone entirely.
    expect(screen.queryByLabelText('App proposals awaiting review')).toBeNull();
  });

  it('a list error afterwards does not delete the verdict notice', async () => {
    const list: PendingProposal[] = [proposal({ ops: [op] })];
    let failing = false;
    mockList.mockImplementation(
      () =>
        ({
          data: failing ? undefined : { proposals: list },
          error: failing ? new Error('transient boom') : null,
        }) as unknown as ReturnType<typeof useWaveProposalsQuery>,
    );
    mockAccept.mockReturnValue(
      stubMutation(() => {
        list.length = 0;
        failing = true;
        return Promise.resolve({ decision: 'stale', reason: 'heads moved' });
      }) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    render(<ProposalsPanel waveId="w_1" blocks={blocks} />);
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    const alerts = await screen.findAllByRole('alert');
    const texts = alerts.map((a) => a.textContent ?? '');
    expect(texts.some((t) => t.includes('heads moved'))).toBe(true);
    expect(texts.some((t) => t.includes('transient boom'))).toBe(true);
  });

  it('both buttons are keyboard-operable', async () => {
    const acceptStub = stubMutation(() => Promise.resolve({ decision: 'accepted' }));
    const rejectStub = stubMutation(() => Promise.resolve({ decision: 'rejected' }));
    mockAccept.mockReturnValue(
      acceptStub as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    mockReject.mockReturnValue(
      rejectStub as unknown as ReturnType<typeof useRejectProposalMutation>,
    );
    renderPanel([op]);
    screen.getByRole('button', { name: 'Accept' }).focus();
    await userEvent.keyboard('{Enter}');
    expect(acceptStub.mutateAsync).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole('status')).toHaveTextContent('Accepted');

    // …and Reject, via Space, which is the other native button key.
    screen.getByRole('button', { name: 'Reject' }).focus();
    await userEvent.keyboard(' ');
    expect(rejectStub.mutateAsync).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole('status')).toHaveTextContent('Rejected');
  });
});

describe('ProposalsPanel — verdicts are wave-scoped', () => {
  it('a verdict from one wave does not render over another wave report', async () => {
    // `WaveReportPage` switches waves IN PLACE, so the panel must not
    // carry wave A's outcome into wave B.
    const list: PendingProposal[] = [
      proposal({ ops: [{ op: 'delete_block', block_id: 'b_0001', if_rev: 3 }] }),
    ];
    mockList.mockImplementation(
      () =>
        ({ data: { proposals: list }, error: null }) as unknown as ReturnType<
          typeof useWaveProposalsQuery
        >,
    );
    mockAccept.mockReturnValue(
      stubMutation(() => {
        list.length = 0;
        return Promise.resolve({ decision: 'accepted' });
      }) as unknown as ReturnType<typeof useAcceptProposalMutation>,
    );
    const { rerender } = render(
      <ProposalsPanel waveId="w_1" blocks={blocks} />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Accept' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Accepted');

    rerender(<ProposalsPanel waveId="w_2" blocks={blocks} />);
    // Wave 2 has nothing pending and nothing to report → no panel at all.
    expect(screen.queryByLabelText('App proposals awaiting review')).toBeNull();
    expect(screen.queryByText(/Accepted/)).toBeNull();
  });
});
