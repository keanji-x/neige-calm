# Brief: feat(#679) PR5 — FakeProvider + FakeRoot + in-memory full-loop e2e (zero processes)

Single commit. NO `Co-Authored-By`. Summary < 25 lines. **Test-only: NO production change, NO migrations,
NO schema/wire/trait changes. `calm-exec/src` stays implementation-free.** Grep the real signatures before
coding (the pseudocode below is a guide; adapt to the actual `WorkerProvider`/`AgentReactor`/`DecisionSink`/
`Observation`/`DecisionIntent` shapes in calm-exec + calm-types).

## Goal
Prove the dispatch→worker→root→lifecycle loop converges with ZERO processes, AND **defend** that contract
through the PR6/PR7 rewrites by committing through the REAL gated entrance (`commit_decision`) the production
harness will use — not a pure recorder that would stay green even if PR6/PR7 broke the gate.

## Placement
- Fakes → NEW `crates/calm-truth-test-harness/src/fakes.rs`, re-exported from its `lib.rs`. Add
  `calm-exec = { path = "../calm-exec" }` to `calm-truth-test-harness/Cargo.toml` (acyclic).
- e2e + contract tests → NEW `crates/calm-exec/tests/full_loop.rs`, each a thin `#[tokio::test]` delegating
  to a `pub async fn` in the harness (mirror the existing `calm-exec/tests/truth_conformance.rs` delegation).
- `cargo tree --depth 2 -p calm-exec --all-targets` must keep sqlx OUT of calm-exec's non-dev tree.

## Fakes (in fakes.rs)
1. **FakeProvider** (impl `WorkerProvider`): `kind()="fake"`; `session_mode()=Ephemeral`; `probe_liveness()`
   returns the next entry from a scripted `Vec<Liveness>` (builder `with_probe_script([...])`, tracks a
   probe-call counter); `interpret_exit(evidence)`: exit_code 0 ⇒ `ExitInterpretation::Completed`, nonzero/
   signal ⇒ `Failed{reason}`; `resume()` = the trait default (Err). No real spawn/PTY.
2. **FakeRoot** (impl `AgentReactor`): `principal()` returns the root `Principal::Agent{session_id,wave_id,cove_id}`;
   `react(obs)` matches a scripted first-match table `Vec<(ObsMatcher, Vec<DecisionIntent>)>` (builder
   `.on(matcher, intents)`); non-matching ⇒ empty vec. For PR5 the only entry: TaskCompleted ⇒
   `[DecisionIntent::LifecycleTransition{wave_id, to: Done, agent_message: Some("converged")}]`.
3. **RecordingDecisionSink** (impl `DecisionSink`): `Mutex<Vec<(Principal,DecisionIntent)>>`; `commit` pushes +
   `Ok(())`; `committed()` reader. (Cheap contract tests only.)
4. **GatedDecisionSink** (impl `DecisionSink`) — THE DEFENDER: holds `repo: SqlxRepo, bus: EventBus,
   write: WriteContext, gate: Arc<G: DecisionGate>`. `commit(principal, intent)`:
   - map `Principal` → `ActorId` (Agent ⇒ the wave's actor; Kernel ⇒ `ActorId::Kernel`) + `EventScope::Wave(wave_id)`.
   - `LifecycleTransition{wave_id,to,agent_message}` ⇒ read current lifecycle (`from`), build
     `Event::WaveLifecycleChanged{...from,to,agent_message...}`, and call the SAME `commit_decision(repo, gate,
     actor, scope, None, &bus, &write, event, |tx| wave_update_tx(tx, ...set lifecycle=to...))` that T1 tests use.
   - all other `DecisionIntent` variants ⇒ `Err(CoreError::...)`/`unimplemented` (only the Done loop is wired in PR5).
5. **FakeObservationSink** (impl `ObservationSink`): in-memory `Mutex<Vec<...>>` queue; `deliver` enqueues;
   redelivery of the same `Some(envelope_id)` enqueues once (idempotent), `None` always enqueues. (Off the
   e2e critical path — seam-completeness contract test only.)

## The e2e (pub async fns in the harness, invoked from calm-exec/tests/full_loop.rs)
**`full_loop_dispatch_to_lifecycle_done`**:
1. `(repo, wave_id) = seeded_repo()`; get cove_id. Seed a root `WorkerSession` (Planner) +
   `set_wave_root_session_for_test(repo, wave_id, Some(root_sid))`. Drive the wave Draft→…→Reviewing via
   `wave_update_tx` so the `Reviewing→Done` edge is FSM-legal. Assert lifecycle == Reviewing.
2. Build `FakeProvider::new().with_probe_script([Idle, Exited{...}])`, `FakeRoot::for_wave(root_sid,wave_id,cove_id)
   .on(TaskCompleted, [LifecycleTransition{..Done..}])`, `GatedDecisionSink{repo,bus,write,PermissiveGate}`.
3. DISPATCH leg (zero-process): `verdict = provider.interpret_exit(&session, &ExitEvidence{exit_code:Some(0),
   signal_killed:false, observed_at_ms:now, source:Probe}, &ctx).await?`; assert `verdict==Completed`. Map to
   the kernel convergence observation INLINE (the same pure event→observation mapping; do NOT instantiate the
   dispatcher/scheduler — keeps it L1, no calm-server dep): `obs = Observation::TaskCompleted{idempotency_key:"t-1", result: json!({})}`.
4. REACT: `intents = root.react(&obs).await?`; assert `intents == [LifecycleTransition{..Done..}]`.
5. COMMIT: `for intent in intents { sink.commit(&root.principal(), intent).await? }`.
6. CONVERGE: `repo.wave_get(wave_id)` lifecycle == `WaveLifecycle::Done`; `events_since(0)` contains EXACTLY
   one `Event::WaveLifecycleChanged{to:Done}` (T1 coupling held end-to-end through the real gated entrance).

**`full_loop_cross_principal_denied`** (defends PR7b): commit the SAME Done intent as a non-root principal
through a root-checking gate (reuse the harness `DenyOnRoot` pattern) ⇒ `Err` (Forbidden); assert lifecycle
UNCHANGED + ZERO new events (the gate, not the sink, owns authority).

## Contract tests (harness pub fns → calm-exec/tests/full_loop.rs)
- FakeProvider: scripted probe order + probe-call count; interpret_exit Completed/Failed rules; session_mode Ephemeral; resume() Err.
- FakeRoot: first-match returns scripted intents; non-matching ⇒ empty.
- FakeObservationSink: same `Some(envelope_id)` redelivery enqueues once; `None` always enqueues.

## Acceptance gate
- `cargo fmt --all -- --check` · `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p calm-exec -p calm-truth-test-harness` green (the 2 e2e + the contract tests).
- `cargo tree --depth 2 -p calm-exec --all-targets | grep sqlx` empty (sqlx only via the dev-dep harness).
- `calm-exec/src` unchanged; zero migrations; no golden / `generated-events.ts` / matrix change.

## Commit message
`feat(#679): PR5 — FakeProvider + FakeRoot + in-memory full-loop e2e (zero-process wave convergence via the gated entrance)`
