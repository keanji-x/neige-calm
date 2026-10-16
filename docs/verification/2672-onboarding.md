# Packaged onboarding validation (#2672)

The intended user already uses an AI coding CLI and has no Neige knowledge.
The implementation provides fresh Linux package installation, discoverable
working methods and prerequisites, and English/Chinese first-use guidance.

Review tier: **L2**. Fresh setup initializes private persisted installation
identity and credentials and activates verified release paths. Two independent
read-only review channels converged on source `bfc2dd523ac5942cc4a7fa91a8f1053fac542759`
with no remaining blocking finding and all five architecture priorities checked.
The integration base is `64ca8c1e7ea1cc997e657b3c612cb0aa7716180c`.
The final frontend run used `40c1587ec`; frontend code is unchanged in the final
installer-only descendant. The audit document is a later documentation-only commit.

## Test environment

Fresh-context agents received only the archive, shipped user guidance, runtime
locations and user goals. Each installation used a separate clean Debian Linux
container with no Rust, Node or Go. The current Codex executable and auth file
were mounted read-only; its parent home remained private and writable. No host
home, application data or development repository was mounted. The app had a
private PID namespace, no ordinary network interface, and a fenced provider-only
proxy. Explicit deny canaries rejected host service, private and link-local
addresses. A loopback UI bridge let real Chromium operate the installed app.

All repository edits and actual model calls happened inside the containers.
Host Playwright only drove the user interface. No external Issue, PR, push or
merge was performed by the product exercises. Test Git identity was explicitly
authorized and scoped only to the copied fixture repository.

## First fresh-user pass

Alpha.2672 was built from `c29a978edc05a5be07277c3bf2e82c9474d74b00`.
The first agent verified the archive and used interactive hidden-password
setup, owner login and the printed foreground command. It completed a real
Investigation with file-linked evidence, opened the code viewer, refreshed the
saved Report, made a real bounded `--help` Small change and independently
checked the actual diff and Bash tests. It enabled Development through the
missing-plugin link, returned to the preserved draft and confirmed an empty
Issue and automatic merge off. After restart it found closed work, reopened
the original Investigation and completed a real follow-up without losing the
old Report.

Its own-word explanation correctly distinguished Area/Track/Report, template
choice, optional Issue, plugin prerequisite, human merge approval, Recipe
semantics and restart. Its confusion improved the guide: save proxy settings
then restart/recheck; a Planner may close completed work; Development accepts
direct requests; local uncommitted work is not automatically handed to another
Track. Guided handoff remains a separate design in #2680.

## Second fresh-user pass

Alpha.2674 was built from `57852302c1ac2a0d47203af842c1b3ee9731459d`.
A new agent again completed installation, real readiness, file-linked
Investigation, an actual local `--help` change and independent checks. It
verified plugin repair retained the draft and started Development with no
Issue and automatic merge off.

This pass exposed two useful barriers:

- Proxy rows could show a false remote-conflict warning when only the sibling
  setting changed. A production-component regression was red (one failure,
  31 siblings passed). The own-field comparison fixes that proven condition;
  the relevant 41 tests and real Chromium checks pass. This does not establish
  every timing cause of the original transient message. See #2685.
- Development implemented and tested README changes but automatic checkpoint
  failed because the fresh environment had no Git identity. The agent retained
  the work and did not invent an identity. After explicit fixture identity
  authorization, supported product `git_commit` recovered commit
  `8884e1ffb59c7bc9920c00a0b2eecb3fb9a6bd68`. Independent scope, clean-state,
  default/name/empty-output and test checks passed. The Report accurately said
  both automatic attempts failed and direct verification passed; this pass
  does **not** count as a successful managed gate.

The guide now explains the Git prerequisite. Development checks author and
committer before dispatching implementation and asks for user configuration or
explicit repository identity authorization. The policy lives in its template
body, which is actually snapshotted into Planner startup. A real compiler/context
regression failed for metadata-only placement and passed after the move; no
persisted context or generic runtime policy was expanded.

The second agent also verified normal stop/start, retained reports, Show closed,
Reopen and a real 16-second continuation of the original Investigation. Its
conceptual explanation correctly included preservation of local edits and
choosing Development early when later PR delivery is likely.

## Final package and acceptance

Private QA package: `0.1.0-alpha.2676`, source
`bfc2dd523ac5942cc4a7fa91a8f1053fac542759`, native Linux x86_64, glibc 2.36.
The actual release builder completed on a clean frozen checkout, ran the real
package command and production frontend build, and emitted an archive,
checksum and BUILD.json. `GOFLAGS=-buildvcs=false` disables optional Go VCS
auto-stamping in this environment; the package records the exact source and
manifest hashes independently. No release tag or public release was created.

The third fresh-context agent installed alpha.2675 and confirmed its runtime
source. Before any implementation task or file change, Development asked for
both missing Git identities; independent status/diff were clean at original
`fb56a326adbc661d79489453486c70dc1c86d42a`. The agent answered through Questions
with the explicitly authorized fictional, repository-only identity. The product
configured it locally; no host/global Git change or terminal rescue occurred.

The first managed task failed because its generated gate assumed Python, absent
in the minimal runtime. The Planner retained the work and automatically replaced
that check with Bash. Actual normal UI GET data—not only report prose—confirms
`readme-bash-gate` done with `gateResult.passed=true`, and independent read-only
`readme-independent-review` done. Delivered/gated head is
`8545c4144948c4437c62d3febfaa3b78c8775511`: README only, original scripts unchanged,
correct fixture author/committer, clean status, four documented examples and
existing tests verified. The original failed task remains legitimate history.

This real review exposed #2696: strict frontend decoding omitted the existing
optional `result_contract` field. A valid `review-v1` block became unsupported,
lost its server done status and appeared Waiting. Production regressions were
red for Codex and Claude, then green with the single owning schema field.
Terminal/tombstone arms remain strict; no client registry was introduced.
Sanitized real task declarations and diagnostics reproduce the same cause.
The corrected real-data UI showed Failed 1 / Completed 2, opened the review
reference/history, and preserved report text, declarations and head.

Independent review also found that a colon in an installation root could pass
setup but fail the runtime PATH encoder. The real CLI regression first returned
unexpected success. Setup now uses the same standard `join_paths` constraint
before parent creation. All 16 setup-focused tests pass, including refusal with
no parent writes and an allowed colon in an unrelated config filename.

The final alpha.2676 archive checksum passed after transfer. Its actual binary
refused that invalid root without writes; a fresh automated private installation
on separate internal ports booted with matching source and web version. The
existing isolated alpha.2675 installation then received the documented admin
upgrade: dry-run preserving, apply committed, `allowBreaking=false`, with its
`neigeApp` unit explicitly deferred per protocol. No original host instance
was changed.

The cold agent then retested the **installed package** on its normal UI port,
without cache bypass, model requests or settings edits: About and the natural
version response show `bfc2dd523`; Failed 1 / Completed 2; review reference and
attempt history show the same original attempt 1 done. The report and all three
declarations match their earlier bytes, with the same delivered head. No update
or reload prompt was needed. Screenshots 30–33 and sanitized packaged GET
evidence preserve this final check.

The agent correctly explained all requested concepts in its own words. Its
initial uncertainty about save feedback was resolved by same-operation captures:
the visual check and accessible Saved status appear briefly; reopening confirmed
persistence. About's internal Version `0.0.0` is distinct from the archive's
release ID, as the shipped maintainer runbook documents.

A supplemental proxy edit during active work exposed the separate #2693 recovery
limitation: later Planner state remained Working/Stopping while its worker was
done; a documented foreground restart preserved the candidate/report and
released the queued continuation. Source audit supports a shared-provider
replacement/reconciliation gap, but the exact triggering log was truncated, so
this exercise does not prove every causal step. A correct fix needs lifecycle,
generation and registry ownership work; no such fix is claimed here. The guide
now says to configure network before work and wait for active work before later
changes. This intentional mid-run edit is distinct from the normal first-setup
path, which already configured and restarted before the first Track.


## Automated verification

- `scripts/local-ratchet-gates.sh`: all five text gates passed.
- `scripts/local-contract-gates.sh`: plugin 195, server/types 178 and integration
  13 tests passed.
- `scripts/local-rust-gates.sh --quick --base 64ca8c1e7ea1cc997e657b3c612cb0aa7716180c`:
  formatting, Clippy and selected OpenAPI drift checks passed. Broad Rust suites
  remain CI-owned and were not run locally.
- Focused setup CLI/unit/preflight replay: 16 passed; punctuation config
  roundtrip: one passed; shared runtime socket helper: one passed.
- Complete route-unit filter `routes::tracks::tests`: 19 passed, including
  Development's compiled startup identity instruction.
- `cargo nextest run --locked -p calm-server --test track_suite -E 'test(track_templates_read) | test(template_startup_context)'`:
  14 passed through actual template APIs and startup snapshot paths.
- `npm run gen:api`: actual generator passed 128 binding checks and emitted
  the required template metadata schema; wire output did not change.
- Frontend install, lint and production build passed. Final full
  `npm test -- --maxWorkers=2`: **5,317 passed, one skipped, zero failures**.
  An earlier concurrent full run had three asynchronous UI assertion/timeouts;
  the unchanged three files replayed 116/116 green, then the complete bounded
  run passed. Both failed and passing evidence were retained; no test was
  disabled or timeout increased.
- Nine affected Chromium browser files: 60 passed. Final Settings surface:
  three browser tests passed. Focused Settings/provider contracts: 41 passed. Contracted-task
  decoder/view checks: 197 passed; task/reference Chromium checks: 11 passed.
- `./e2e/run.sh` selected all three credential-free Tier 1 cases in a disposable
  empty HOME: two passed, one skipped, zero failed. The dormant-path case
  explicitly skipped because its live harness could not reach that condition.
  No host credentials or real model requests were available in this stack.
- Predicted single-factor production mutations pinned exclusive setup,
  integrity, URI refusal, template-owner readiness and plugin invalidation.
  Complete observable red sets were compared, every mutation restored, and
  green checks replayed before integration.

The full frontend check initially also found 29 positive compatibility fixtures
using an older literal version. They now use the shared current version;
negative historical scenarios remain explicit. Untouched baseline checks and
the corrected full suite confirmed that diagnosis.

## Evidence boundaries and follow-ups

Each pass kept its chronological report, screenshots, actual checkout checks
and observed failures in a separate private evidence directory. Credentials,
raw auth logs and host application data are excluded. Containers and browser
processes are retired after evidence capture.

An initial mixed-version artifact was rejected and is not a successful pass.
A build whose source changed during review correctly refused to publish.
Interrupted preliminary builds were not validation artifacts.

This exercise covers Linux and the supplied Codex environment. Claude login,
actual GitHub publication/merge, phone use and remote connectors are not
claimed. Upgrade coverage is limited to the compatible QA update above. No released database migration was changed. #2680 tracks guided
local-result handoff; #2693 tracks live-provider lifecycle reconciliation. #2687
records a separate pre-existing Planner-duration warning hypothesis that still needs its own reproduction; it is not a claimed
fix or blocker of this onboarding path.
