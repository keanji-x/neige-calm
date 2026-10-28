# System Assistant event decoder contract repair

PR #3311 exposed a missing handwritten decoder branch for the already registered
`assistant.input_requested` event. The Planner approved this narrow core/api
contract extension and the required pure core/events policy declaration.

The API owner mirrors `Event::AssistantInputRequested` in
`crates/calm-types/src/event.rs` and the kebab-case `SystemAssistantSource` in
`crates/calm-types/src/system_assistant.rs`: required string card, track, original
worker, subject and text fields; source `today-summary`; unsigned 32-bit version.
Wire decoding preserves the full u32 range, including zero; input acceptance
rules such as version 1 and nonempty subject remain in the Rust owner.

The existing discriminated union and bidirectional GeneratedEvent assertions
remain authoritative. The core/events owner declares a reasoned noop: the request
itself changes no queried row; subsequent harness events expose visible data.
No enqueue, activation, producer, summary UI or runtime caller is added.

Review tier: L2 because this changes a frozen public decoder's behavior. The
Planner must arrange two fresh full reviews; earlier reviews do not cover it.
The focused API and invalidation tests cover acceptance, rejection and empty
cache effects. The kernel owns the seven declared gate steps. No generator input
or generated artifact changes, new registry, permanent check or test-name list
is needed. Ordinary new events continue to update the existing union and policy
map; this repair adds no separate maintenance list or CI invocation.

Full-suite acceptance remains required. Earlier monolithic frontend gates were
red; the baseline comparison also reproduced the first-message queue timeout
followed by empty DOM, plus three other timeouts. Both diagnostic runs reported
worker errors, so neither proves complete coverage. The baseline's shared
`node_modules` symlink also caused a vendor-path style-check artifact. Raw logs
and limitations remain in `/tmp/pr3311-suite-diagnosis/diagnosis.json` and its
referenced logs; the initiating timeouts and worker exit causes are unproven.
These failures are not waived or claimed fixed by the decoder repair.

The Planner approved the existing CI partition: all `platform-independent` and
`web-dom` files in three independent processes, sequential locally, using
`npm test -- --shard N/3 --maxWorkers 6` for N = 1, 2, 3. The worker cap limits
shared-host scheduling without changing timeouts, assertions, skips, selection
or CI configuration. Vitest 4.1.10 `list --filesOnly --json` discovers 378 files;
that command does not itself apply sharding. Comparing its output with Vitest's
actual configured sequencer yields three disjoint 126-file sets whose union
equals discovery, each file exactly once. Gate execution must still account for
every file, require every shard exit zero and reject worker errors. A green run
proves this scheduling's coverage, not the cause of the earlier monolithic red.

Frozen paths require exact-path `OWNERSHIP-CHANGE` trailers using the template in
`fe/tools/ownership/README.md`: `fe/core/api/schemas.ts`,
`fe/core/api/schemas.test.ts`, `fe/core/events/invalidation-plan.ts`, and
`fe/core/events/invalidation-plan.contract.test.ts`. Preserve the existing
generated wire trailer from #2671 in the PR squash body.
