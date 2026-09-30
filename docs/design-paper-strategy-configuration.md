# Separate paper account connections from strategy snapshots

## Outcome

Plugin settings contain only the paper account connection (account number and
broker HOME, plus advanced executable/polling controls). The saved Recipe owns
the research/trading method. A conversation in a strategy Track proposes typed
research, symbol and risk parameters; its Track identity comes exclusively from
the host. The human operator approves an exact immutable proposal before tools
can record trading decisions. No defaults grant trading authority.

This slice reuses the existing interactive operator approval surface. It does
not introduce a browser order/configuration authority route or a generic plugin
tool-call permission. The native Report shows proposed/approved settings. Order
submission/cancellation still uses the broker's separate native confirmation.

## Boundaries

- Account settings: account_no, broker_home, cli_path, poll_seconds. Broker HOME
  and executable remain explicit trusted configuration, never agent arguments.
- Strategy settings: research_root, symbols (a JSON array rather than a scalar
  JSON string), max_order_usd, max_portfolio_usd, max_trade_risk_usd,
  quote_max_age_seconds and max_price_deviation_bps. Required risk values have
  no implicit defaults. The Recipe directs the agent to obtain user choices.
- paper.strategy proposes settings, with no owner Track or account argument.
  Proposal identity includes the host Track and exact typed settings. Repeating
  the same proposal is idempotent; changed settings produce a new identity.
- Only the interactive operator can approve. Agent tools cannot approve, supply
  confirmation metadata, change account binding or submit/cancel orders.
- This version retains one approved strategy Track per dedicated paper account.
  Cross-Track proposals cannot seize an already bound account. Multi-strategy
  allocation requires a separate shared-account design; separate independent
  ledgers must not each claim the whole broker account.
- Approved settings are persisted independently of mutable Recipe/report prose.
  Revising the same Track requires explicit approval and no unresolved decisions
  or open trades. A broker identity/position/order reconciliation runs before
  approval. All operations serialize through an account-level process lock;
  stale previews are rejected across an approved configuration revision.
- Proposal journal and active binding live in a new strategy.sqlite3 alongside
  the existing ledger.sqlite3. Existing ledger schema/data are not rewritten.
  Released v1 installations require an explicit interactive import of their
  saved full configuration; never infer old limits or discard historical data.

## User flow

1. Enable the account plugin after configuring the existing paper login.
2. Create one long-lived Track from the paper portfolio Recipe. Discuss the
   strategy and its source/limits there; paper.strategy automatically associates
   the proposal with this Track and publishes its settings in the Report.
3. Run the provided operator approval command. It displays the exact account,
   Track, proposal revision and limits; blank/noninteractive input cannot approve.
4. Continue research/decisions and the existing supervised paper order loop.

No live order, credential change, production ledger migration or choice of user
risk limits is part of development verification. Deployment may install the new
account-only plugin and Recipe, retaining the verified existing connection;
strategy choices remain unapproved until supplied and confirmed by the user.

## Acceptance

- Account-only initialization succeeds with zero strategy and no broker writes.
- Production stdio tools propose/inspect using host identity; no approval tool
  is advertised or accepted. Missing/foreign Track and forged authority fail.
- Approval pins exact account/Track/configuration, survives process restart,
  rejects stale proposals/previews and cannot discard unresolved broker state.
- Existing full entry/partial fill/restart/exit/review tests stay green.
- Explicit legacy import preserves ledger bytes/history, checks original binding
  and refuses missing/mismatched legacy settings.
- Native Report projections show configuration/readiness without claiming that
  a configured strategy has submitted an order. Desktop/mobile host checks use
  deterministic fixtures, never live credentials or real Codex E2E.
- Mutation-check approval/ownership fences; two independent fresh full-diff
  reviews must converge before delivery.
