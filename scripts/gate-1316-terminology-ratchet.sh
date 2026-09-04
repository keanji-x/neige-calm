#!/usr/bin/env bash
# #1316 S0 — retiring-vocabulary ratchet.
#
# WHY THIS SHAPE AND NOT #1209's
#
# `scripts/gate-1209-template-rename-residual.sh` is the end state this repo
# already knows how to run: a whole-repo scan with a per-file allowlist where
# every entry states why it is there and pins how many lines it may match. That
# shape works because `workflow` was down to ~30 files when the gate landed.
#
# #1316 starts from ~10.9k occurrences for `cove` and ~26.8k for `wave`
# across ~900 files. A per-file allowlist of that size would be unreadable and
# nobody would ever check an entry's reason, which is precisely the fig leaf the
# #1209 header warns about. So S0 ships the OTHER standard shape — a ratchet —
# and S6 converts it to the #1209 allowlist form once the counts are small.
#
# THE RULE, BOTH DIRECTIONS
#
# A BASELINE IS ONLY VALID FOR THE TREE IT WAS GENERATED ON
#
# Merging `main` into a slice branch invalidates the numbers, in BOTH
# directions, and this gate will say so. It happened to this very PR: merging
# `main` moved `wave/crates` +144 and `cove/crates` -2, and the gate went red on
# an unmodified script. Regenerate after every merge of upstream, and treat a
# baseline generated before a rebase as expired evidence rather than as a number
# to carry forward.
#
# That regeneration is not a loophole, but only because of an ordering fact: on
# `main` this gate is always live, so `main` can never rise. Every count a merge
# brings in was already gated at the moment it landed. The one tree where that
# is not yet true is this PR itself, which introduces the gate.
#
# THAT ORDERING FACT IS FALSE AS STATED, AND #1448 IS THE PROOF. A PR's CI runs
# against a merge ref built BEFORE it lands. If two PRs are in flight and the
# second adds vocabulary the first's baseline did not count, both go green
# separately and `main` rises the moment the second one merges. #1393 rode
# through exactly that window: it added 20 `runtime_id`, 1 `spec` and 1 `wave`
# occurrences without touching the baseline, `main` went red on itself, and
# every subsequent PR inherited the failure through its own merge ref. So the
# honest statement is narrower: `main` can rise, and when it does, the gate
# reports it on `main` and on every PR at once. The repair is a commit ON
# `main` that renames what can be renamed and argues the rest line by line —
# never a silent `--update-baseline` on a branch that happens to be red.
#
#   actual > baseline  => FAIL. New retiring vocabulary entered the tree.
#   actual < baseline  => FAIL. The ratchet must be tightened; run
#                         `--update-baseline` and commit the new numbers.
#
# The second direction is not pedantry. A baseline left at an old, higher number
# silently re-permits every occurrence the slice just removed. A ratchet that only
# checks one direction is a ratchet with no pawl.
#
# WHAT EACH PATTERN COVERS, STATED HONESTLY
#
# `cove`
#   `cove` IS A SUBSTRING OF `recover`, `discover`, `cover`, `coverage`.
#
#   The first version of this header said "this project's coinage, no
#   ordinary-English use in scope" and paired it with the bare substring
#   `(?i)cove`. That was wrong, and it shipped: 1871 of the 10672 baselined
#   `cove/crates` occurrences — 17.5% — were `recovery` (568), `recover` (240),
#   `recovered` (196), `covers` (140), `coverage` (61), `discover`, and friends.
#
#   Three consequences, all real, none hypothetical:
#     * the ratchet blocked ordinary English — writing "recovery" in a new
#       comment under `crates/` raised the cell and failed CI;
#     * the baseline was inflated by ~17.5%, so the numbers did not mean what
#       the header said they meant;
#     * S6's "drive it to zero" acceptance was unreachable by construction,
#       because `recover` can never leave this codebase.
#
#   This is the SAME error as the `wave` one below, made twice: a universal
#   negative about English collisions asserted without executing anything.
#   The pattern is now boundary-anchored per case, and every branch below was
#   checked in both directions against the real tree.
#
# `wave` — same class, caught earlier by the review channel
#   The original claim ("no `wavelength`, no `microwave`, no `waveform`") came
#   from filtering `[a-z]*wave[a-z]*` through an exclusion regex ending
#   `wave[a-z_]*$`. That trailing clause swallows `waved`, so the filter
#   excluded exactly the English forms it existed to surface.
#
#   ONE RAISE, TAKEN ONCE BY #1448, AND WRITTEN AS AN ENUMERATION. `wave` has
#   no raise CLASS and this note does not open one: it admits exactly one
#   line, the count below is what it buys, and the list is closed.
#
#     crates/calm-server/tests/cases/task_projection_acceptance.rs  6 ->   7
#                                                        net     749 -> 750
#
#   The added occurrence is `neige://wave/{track_id}#{block_id}` — the report
#   link SCHEME, i.e. a wire literal. It is not our noun in prose and it is not
#   an identifier, and it has no synonym: `track_report_blocks/contracts.rs`
#   pins `"pattern": "^neige://wave/[^/#]+#b_[0-9a-f]{4}$"` into the MCP tool's
#   JSON schema, and `0081_wave_to_track.sql` records that this one scheme was
#   deliberately left un-renamed when everything else moved to `track`. Writing
#   `neige://track/` in a fixture is not a rewording, it is a value the schema
#   rejects. Retiring the scheme is a wire break with a migration — its own
#   slice, not a slice that happens to add one fixture. A NEW `wave`
#   identifier, or a new sentence of prose, does not inherit this raise.
#
# HOW THE TWO PATTERNS ARE ANCHORED, AND WHY THEY DIFFER
#
#   Lowercase, both words: `(?<![a-zA-Z])(cove|wave)s?(?![a-z])`
#     A LETTER BEFORE means English (`recover`, `discover`, `handwave`).
#     A LOWERCASE LETTER AFTER means English (`cover`, `covered`, `waved`,
#     `wavering`). A following UPPERCASE letter is ours — camelCase
#     (`coveConversations`, `onWaveRoute`) — so the lookahead is `(?![a-z])`,
#     not `(?![a-zA-Z])`. Getting that wrong drops real call sites silently.
#
#   Capitalised, both words: `(Cove|Wave)s?(?![a-z])`
#     `CoveId`, `NewCove`, `WaveCoveCache`, `WaveId` match; `Coverage`,
#     `Covered` do not.
#
#   Upper case — AND HERE THE TWO WORDS GENUINELY DIFFER, measured:
#     `COVE[A-Z]+` in scope is `COVERY`, `COVERABLE`, `COVERED`, `COVER`
#       (all fragments of RECOVERY/RECOVERABLE) plus `COVES`, which is ours.
#       So cove needs `COVES?(?![A-Z])`.
#     `WAVE[A-Z]+` in scope is `WAVECREATE`, `WAVEWORKSPACE`, `WAVEROW`,
#       `WAVENAME`, `WAVEGLYPH`, `WAVES` — every one of them ours (oracle
#       capability/invariant ids). So wave takes a bare `WAVE`; applying
#       cove's `(?![A-Z])` here would have silently dropped 88 oracle ids.
#     Symmetry would have been wrong in both directions. The branches were
#     measured, not reasoned about.
#
#   What is still over-broad, knowingly: the bare word followed by a space.
#   `wave through` / `hand-wave` (= to let something pass unchecked) is
#   ordinary English this codebase's comments reach for, and no regex separates
#   it from our noun. It is counted, like the bare `spec` case below. Writing
#   "would wave through" in a new comment fails this gate; the honest response
#   is a different word ("would let through"), not a `(?<!hand-)` carve-out
#   that would also exempt real occurrences sitting next to those letters.
#
# `spec` — the PLANNER-AGENT sense (#1316 B class)
#   Narrowed three ways and STILL over-broad, deliberately:
#     * `(?<![a-z])spec(?![a-z])` excludes `specific`, `specifier`,
#       `specification`, `inspect`.
#     * `spec_x` / `x_spec` catch the identifier forms (`spec_card_id`,
#       `spec_thread_id`, `spec_task_ceiling`, `event_spec`).
#     * `.spec.ts` / `.spec.tsx` are EXCLUDED by lookahead. That is the
#       Vitest/Playwright filename convention on 735 lines; it is not ours and
#       renaming it is not in scope for any slice.
#   What remains over-broad: the bare word in prose about an OpenAPI spec or a
#   cookie spec. No regex separates "the spec agent" from "the OpenAPI spec", so
#   the ratchet counts both. Consequence to accept knowingly: adding the phrase
#   "OpenAPI spec" to a comment in `crates/` raises that cell and fails this
#   gate. The honest fix at that moment is to write "OpenAPI schema" (which is
#   what the repo calls it everywhere else) — not to widen the pattern.
#
#   THAT ADVICE COVERS PROSE ONLY. "Say it differently" presupposes a synonym
#   exists. For a PERSISTED VALUE there is none: `declared_by` /
#   `tombstoned_by` store the literal string `"spec"`, that literal is what is
#   already in every production database, and
#   `normalize_task_privilege_fields` writes exactly those bytes. Writing
#   `"planner"` there is not a rewording, it is a wrong program — the row no
#   longer matches the rows beside it, and the ownership checks that compare
#   against `"spec"` stop firing. Changing it needs a data migration, which is
#   the Spec -> Planner slice's job, not a slice that happens to add a caller.
#
#   So this ONE class admits a baseline RAISE with a stated reason. The
#   criterion, and it is narrow on purpose:
#     * every added occurrence is either the literal value itself or an
#       assertion on that literal value;
#     * NOT ONE added line is prose, and not one is an identifier. A new
#       comment mentioning the spec agent, or a new `spec_thread_id`, is the
#       thing this ratchet exists to stop, and it does not become admissible by
#       riding along in the same commit;
#     * the raise commit message lists the constituent lines one by one
#       (path:line + content) so a reviewer can check that claim without
#       re-deriving the delta.
#   A raise that cannot produce that list is not this case; it is drift.
#
#   A SECOND raise, taken ONCE by the S3 follow-up. It is not a class, and
#   it is deliberately written as an ENUMERATION rather than a criterion: a
#   criterion phrased as "identifiers that name a specification" would let any
#   future `_spec` name buy a raise by asserting pre-S3 provenance, which is a
#   wider contract than the repair needed. So the admitted set is listed, and
#   the list is closed.
#
#   WHY THE REPAIR EXISTS. `spec` is the planner agent here, but it is also
#   the ordinary word for a written specification. S3's mechanical pass hit
#   both, so bindings of the FROZEN plugin field `event_spec` came back named
#   `planner` — a name that disagrees with the wire field it carries, which is
#   precisely what made this class hard to review in the first place.
#
#   AND THE REPAIR ITSELF IS DANGEROUS IN THE SAME WAY. Repairing `event.rs`
#   by line range also rewrote the `EditAuthor` tests, where `"planner"` is
#   the PERSISTED `track.report_edited` author literal and S3's rename was
#   correct. Three tests went red. Repair by binding, never by line range.
#
#   THE ADMITTED SET, as PER-FILE COUNTS rather than a description. A
#   description ("bindings whose value is a `ForgeEventSpec`") reads as a
#   standing permission — a later commit adding three more such bindings
#   satisfies it word for word. Counts cannot be re-spent:
#
#     crates/calm-server/src/operation/forge_action_adapter/mod.rs   17 ->  58
#     crates/calm-server/src/mcp_server/transport.rs                  9 ->  12
#     crates/calm-types/src/event.rs                                  7 ->  17
#     crates/calm-server/tests/fixtures/stub-plugin-forge-action/…    5 ->   6
#     crates/calm-server/tests/mcp_plugin_forge_action.rs             2 ->   3
#                                                          net      446 -> 502
#
#   Reproduce with the combined pattern this script builds, occurrence-counted
#   per file, against `origin/main`. If a later commit needs any of these cells
#   higher, that is a NEW raise needing its own argument; it does not inherit
#   this one. A NEW `_spec` identifier is the thing this ratchet exists to stop
#   and does not become admissible by resembling something above.
#
#   WHAT DID *NOT* MOVE THE CELL, stated because getting this wrong once
#   already made this file misreport its own numbers: `GateSpec`,
#   `FixtureSpec` and `PrincipalSpec` were repaired in the same commit and
#   contribute ZERO — the lowercase-branch lookbehind `(?<![a-z])` excludes a
#   `Spec` preceded by a letter, and no other branch matches them. Every
#   rewording that had a good alternative was taken instead and cost nothing:
#   prose went to "test" or "specification", `anchorSpecs` to `anchors`,
#   JSON-schema property bindings to `schema`, a fixture parameter to
#   `fixture`.
#
#   The per-line evidence rule from the first class applies unchanged, and the
#   list must be regenerated against the FINAL diff — a list produced before a
#   later commit or a merge is expired evidence, not an audit trail.
#
# REQUIRED-FIELD INITIALISERS ON TYPES THIS SLICE DOES NOT OWN
#   The `spec` note above splits the world in two: prose can be reworded, a
#   persisted literal cannot. There is a THIRD case, and the `runtime_id` cell
#   below is where it turns up.
#
#   When a slice adds one construction site for a type that already exists, the
#   field NAMES in that initialiser belong to the type definition, which lives
#   somewhere else. The caller does not get to choose them:
#   `crates/calm-types/src/event.rs` declares
#   `Event::HarnessUserMessageEnqueued { runtime_id: String, .. }`, so an
#   initialiser that spells it any other way is not worded differently, it does
#   not compile. Spelling it differently for real means editing the type
#   definition plus every existing construction and every read of it — the work
#   of the matching rename slice, not of a slice whose only involvement is
#   being one more caller.
#
#   So this is the SECOND class that admits a baseline RAISE with a stated
#   reason, and like the first it is narrow on purpose:
#     * every added occurrence is a STRUCT- OR VARIANT-FIELD INITIALISER inside
#       a construction expression, and it is either
#         (i) the FIELD NAME, where that name is declared by a type-definition
#             line this slice's diff does NOT add — note this is per FIELD, not
#             per type: a slice may add other fields to the same type and still
#             count a retiring-name field it did not introduce under this
#             clause; or
#         (ii) a SAME-NAMED LOCAL BINDING handed to that field — and ONLY when
#             the binding itself is not introduced by this slice (its `let` is
#             not in this slice's diff). This branch is the narrow one and the
#             restriction is the whole point: `let runtime_id = new_id();
#             Foo { runtime_id }` inside one slice is TWO fresh retiring-name
#             identifiers, which is exactly what this ratchet exists to stop.
#             A slice that needs a new local there names it something else and
#             spells the field explicitly (`Foo { runtime_id: fresh }`), paying
#             the one unavoidable occurrence instead of two;
#     * NOT ONE added line is prose, and not one is an identifier this slice
#       itself coined. A new comment mentioning a retiring concept, or a fresh
#       `runtime_id`-shaped name minted here, is precisely what this ratchet
#       exists to stop, and it does not become admissible by riding along in
#       the same commit;
#     * the field is REQUIRED by the type. If the type derives `Default` and
#       leaving the field out changes no behaviour, then leave it out — that is
#       not this case. `#[serde(default)]` alone does NOT make a field
#       omissible: it governs deserialization, while a Rust struct literal must
#       still name every field of a type that has no `Default`.
#     * the raise commit message lists the constituent lines one by one
#       (path:line + content) so a reviewer can check all three points above
#       without re-deriving the delta.
#   A raise that cannot produce that list is not this case; it is drift.
#
# `runtime_id` — the RETIRED-ID sense ONLY (#1316 B class, narrowed at S0)
#   `runtime` is far wider than this sense in scope. The umbrella issue
#   originally said "Runtime 退役", which was too wide for its carrier and is
#   now narrowed: the verified claim is that the runtime id IS the worker
#   session id. The single citation for that: `harness/mod.rs` declares
#   `spawn_recovered_harness(runtime: WorkerSessionProjection)` and binds
#   `let runtime_id = runtime.id.clone()`. Both sit outside this file's only
#   `#[cfg(test)]`, and `WorkerSessionProjection.id` is `worker_sessions.id`
#   (`0045_worker_sessions.sql`, `id TEXT PRIMARY KEY`).
#
#   This paragraph deliberately carries ONE citation, and every attempt to make
#   it carry more has produced a falsehood. S4b-0 (#1415) deleted the citation it
#   used to carry (`pub type RuntimeId = String;`); the replacement drafted for
#   it pointed instead at `decision_sink.rs`'s `WorkerSessionId::from(..)` and
#   asserted the harness's value flows into it — there is no reference to
#   `PlannerHarnessAgentReactor` anywhere under `harness/`, so that was false.
#   The version after THAT added a caller census which was also false.
#   If this citation ever dies too, DELETE the sentence rather than reach for a
#   second one — the narrowing decision stands on its own.
#
#   Everything else spelled `Runtime` is
#   a DIFFERENT concept and is not scanned: `PluginRuntimeStatus` (plugin
#   process state), `OperationRuntime` (`operation/driver.rs`), `ProcRuntime`,
#   `runtime_layer`, and ordinary English "fails at runtime".
#
# `harness_item` — the TRANSCRIPT sense ONLY (#1316 B class, narrowed at S0)
#   Same correction. `crates/calm-server/src/harness/` is `registry.rs`,
#   `run_loop.rs`, `lock.rs`, `state.rs`, `snapshot.rs` — a RUNNING agent
#   instance that claims a registry slot (`spawn_recovered_harness`,
#   `try_reserve`), not a log. Renaming those to `Transcript*` would be a worse
#   name than the one it replaces. Only `harness_items` / `HarnessItem` is
#   genuinely a protocol transcript, so only that is scanned.
#   Left open on purpose: the harness registry is keyed by `runtime_id`, i.e.
#   `runtime` / `harness` / `worker_session` are three names for layers of one
#   execution concept. That is a design question, not a rename, and #1316
#   explicitly refuses to smuggle it into a mechanical slice.
#
# SCOPES
#   Ratcheted: `crates` `fe` `docs` `e2e`.
#   Informational only: `web` — owner decision, the legacy bundle is being
#   deleted, so it gets minimal compile-only fixes and must not gate anything.
#   It is still REPORTED so its deletion shows up as a number.
#
# PROVE IT DISCRIMINATES
#   `--selftest` runs the positive/negative pair required of any gate here:
#     * inject `cove_id` into a file in a ratcheted scope  => must FAIL
#     * inject `specification` and "fails at runtime"      => must PASS
#     * inject `foo.spec.ts`                               => must PASS
#   It uses a scratch file it creates and deletes; it never mutates tracked
#   files.

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1

SELF='scripts/gate-1316-terminology-ratchet.sh'
BASELINE='scripts/gate-1316-terminology-ratchet.baseline.tsv'

# term<TAB>pattern. Reasons for each are in the header above.
read -r -d '' TERMS <<'EOF' || true
cove	(?<![a-zA-Z])coves?(?![a-z])|Coves?(?![a-z])|COVES?(?![A-Z])
wave	(?<![a-zA-Z])waves?(?![a-z])|Waves?(?![a-z])|WAVE
spec	(?i)(?<![a-z])spec(?![a-z.])|(?i)spec(?!\.tsx?)_[a-z]|(?i)[a-z]_spec(?![a-z])|AiSpec|SpecHarness|SpecAgent|SPEC_
runtime_id	(?i)runtime[_-]?id|RuntimeId
harness_item	(?i)harness_item|HarnessItem
EOF

RATCHETED_SCOPES=(crates fe docs e2e)
INFO_SCOPES=(web)

# Counts OCCURRENCES, not matching lines.
#
# The first version of this gate used `git grep -c`, whose unit is the matching
# LINE. That left the ratchet trivially bypassable: appending a second
# `cove_id` to a line that already matches does not move a line count, so new
# retiring vocabulary could enter the tree completely green. `-o` emits one
# output line per match, so `wc -l` is an occurrence count and the bypass is
# closed. Occurrence counting is also strictly tighter in the down direction —
# deleting one of two matches on a shared line now registers.
count() { # $1=pattern $2=scope
  local matches grep_status

  matches="$(git grep -P -o -h "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>/dev/null)"
  grep_status=$?
  case "$grep_status" in
    0) printf '%s\n' "$matches" | wc -l ;;
    1) printf '0\n' ;;
    *)
      echo "::error::git grep failed with exit $grep_status while scanning '$2'; refusing to use a partial count." >&2
      return "$grep_status"
      ;;
  esac
}

ensure_baseline_inputs_are_tracked() {
  local untracked

  if ! untracked="$(git ls-files --others --exclude-standard -- "${RATCHETED_SCOPES[@]}")"; then
    echo "::error::could not check ratcheted scopes for untracked files; refusing to update $BASELINE." >&2
    return 1
  fi

  if [ -n "$untracked" ]; then
    echo "::error::refusing to update $BASELINE: git grep would omit these untracked files from ratcheted scopes:" >&2
    while IFS= read -r path; do
      printf '  %s\n' "$path" >&2
    done <<<"$untracked"
    echo "::error::Stage intended files with git add (or git add -N), then rerun --update-baseline." >&2
    return 1
  fi
}

emit_baseline() {
  local found

  echo "# #1316 retiring-vocabulary ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are OCCURRENCES per (term, scope). Only-down is enforced."
  printf '# term\tscope\tcount\n'
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    for scope in "${RATCHETED_SCOPES[@]}"; do
      if ! found="$(count "$pattern" "$scope")"; then
        return 1
      fi
      printf '%s\t%s\t%s\n' "$term" "$scope" "$found"
    done
  done <<<"$TERMS"
}

if [ "${1:-}" = '--update-baseline' ]; then
  ensure_baseline_inputs_are_tracked || exit 1

  if ! baseline_tmp="$(mktemp "$BASELINE.tmp.XXXXXX")"; then
    echo "::error::could not create a temporary baseline next to $BASELINE." >&2
    exit 1
  fi
  cleanup_baseline_tmp() { rm -f -- "$baseline_tmp"; }
  trap cleanup_baseline_tmp EXIT
  trap 'exit 130' HUP INT TERM

  if ! emit_baseline >"$baseline_tmp"; then
    echo "::error::failed to generate $BASELINE; the existing baseline was preserved." >&2
    exit 1
  fi
  if ! chmod 0644 "$baseline_tmp" || ! mv -- "$baseline_tmp" "$BASELINE"; then
    echo "::error::could not replace $BASELINE; the existing baseline was preserved." >&2
    exit 1
  fi
  trap - EXIT HUP INT TERM
  echo "wrote $BASELINE"
  exit 0
fi

if [ "${1:-}" = '--selftest' ]; then
  probe='crates/calm-types/src/_gate_1316_selftest_probe.rs'
  # `git grep` only reads TRACKED paths, which is the right behaviour in CI
  # (everything under review is committed) but makes a plain untracked probe
  # invisible — the first version of this selftest reported a pass for an
  # injection the gate had never actually seen. `git add -N` puts the probe in
  # the index so the probe is subject to the same scan a real commit would be.
  fail_git_dir=''
  trap 'git rm -q --cached --force -- "$probe" >/dev/null 2>&1; rm -f "$probe"; if [ -n "$fail_git_dir" ]; then rm -f "$fail_git_dir/git"; rmdir "$fail_git_dir"; fi' EXIT
  fails=0

  # Baseline generation must fail closed before opening the baseline for write.
  # Otherwise `git grep` silently omits an untracked file and records a count
  # below the tree that will actually be committed.
  baseline_hash="$(git hash-object -- "$BASELINE")"
  printf 'let x = wave_id;\n' >"$probe"
  if update_output="$("./$SELF" --update-baseline 2>&1)"; then
    echo "SELFTEST FAIL: --update-baseline accepted an untracked file in a ratcheted scope"
    fails=1
  elif ! grep -Fq "$probe" <<<"$update_output"; then
    echo "SELFTEST FAIL: --update-baseline rejected an untracked file without naming it"
    fails=1
  elif [ "$(git hash-object -- "$BASELINE")" != "$baseline_hash" ]; then
    echo "SELFTEST FAIL: rejected --update-baseline changed the existing baseline"
    fails=1
  else
    echo "selftest ok: --update-baseline rejects an untracked input and preserves the baseline"
  fi

  printf 'let x = cove_id;\n' >"$probe"
  git add -N -- "$probe" || exit 1

  # A scan error must propagate through emit_baseline, and the atomic update
  # must leave the prior baseline intact. The wrapper fails only `git grep` and
  # delegates every other Git command used by the nested gate invocation.
  fail_git_dir="$(mktemp -d)" || exit 1
  cat >"$fail_git_dir/git" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = grep ]; then
  exit 128
fi
exec "$GATE_1316_REAL_GIT" "$@"
EOF
  chmod +x "$fail_git_dir/git" || exit 1
  if scan_error_output="$(PATH="$fail_git_dir:$PATH" GATE_1316_REAL_GIT="$(command -v git)" "./$SELF" --update-baseline 2>&1)"; then
    echo "SELFTEST FAIL: --update-baseline accepted a git grep failure"
    fails=1
  elif ! grep -Fq 'git grep failed with exit 128' <<<"$scan_error_output"; then
    echo "SELFTEST FAIL: --update-baseline did not report the git grep failure"
    fails=1
  elif [ "$(git hash-object -- "$BASELINE")" != "$baseline_hash" ]; then
    echo "SELFTEST FAIL: failed baseline scan changed the existing baseline"
    fails=1
  else
    echo "selftest ok: --update-baseline propagates scan errors and preserves the baseline"
  fi

  if "./$SELF" >/dev/null 2>&1; then
    echo "SELFTEST FAIL: injected 'cove_id' did not trip the gate"; fails=1
  else
    echo "selftest ok: injected 'cove_id' trips the gate"
  fi

  printf 'The specification is loaded; a bad config fails at runtime.\nSee foo.spec.ts for the case.\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "selftest ok: 'specification' / 'at runtime' / 'foo.spec.ts' do not trip it"
  else
    echo "SELFTEST FAIL: ordinary English tripped the gate"; "./$SELF"; fails=1
  fi

  # The English-substring case that the first shipped pattern got wrong. `cove`
  # lives inside recover/discover/cover/coverage and `wave` inside waved; the
  # bare `(?i)cove` this gate launched with counted 1871 such occurrences in
  # `crates` alone and failed CI on anyone who wrote "recovery" in a comment.
  printf 'Recovery is recoverable: the reaper recovered every uncovered branch it discovers.\nCoverage covers what the audit covered; the reviewer waved it through while wavering.\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "selftest ok: recover/discover/cover/coverage/waved/wavering are not counted"
  else
    echo "SELFTEST FAIL: ordinary English containing 'cove'/'wave' as a substring tripped the gate"
    "./$SELF"; fails=1
  fi

  # The other direction of the same fix. Anchoring the pattern must not drop
  # real call sites: camelCase (a following UPPERCASE letter is ours, not
  # English) and the SCREAMING_CASE oracle ids, of which `WAVE[A-Z]+` has 88 in
  # scope and `COVE[A-Z]+` has none — which is why the two words' uppercase
  # branches are deliberately asymmetric.
  printf 'const x = coveConversations; const y = onWaveRoute;\n// E-CAP-WAVECREATE INV-WAVEROW COVES\n' >"$probe"
  if "./$SELF" >/dev/null 2>&1; then
    echo "SELFTEST FAIL: camelCase / oracle-id forms of our own vocabulary went uncounted — the anchoring is too tight"
    fails=1
  else
    echo "selftest ok: coveConversations / onWaveRoute / CAP-WAVEROW / COVES are still counted"
  fi

  # The unit is OCCURRENCES, not matching lines. Three `cove` on ONE line must
  # move the count by three; under the `git grep -c` this gate originally used
  # it moved by one, which is what made the ratchet bypassable by appending to
  # an already-matching line. Reading the delta is the only way to tell the two
  # implementations apart — both are red here, only one is red for the right
  # reason.
  printf 'cove cove cove\n' >"$probe"
  delta="$("./$SELF" 2>&1 | sed -n 's/.*cove\/crates rose from \([0-9]*\) to \([0-9]*\).*/\2-\1/p' | head -1)"
  if [ -n "$delta" ] && [ "$((${delta}))" -eq 3 ]; then
    echo "selftest ok: 3 matches on 1 line move the count by 3 (occurrence-counted, not line-counted)"
  else
    echo "SELFTEST FAIL: 3 matches on 1 line moved the count by '${delta:-<no rose-from message>}', expected 3 — the ratchet is line-counting again and can be bypassed by appending to a matching line"
    fails=1
  fi

  exit "$fails"
fi

if [ ! -f "$BASELINE" ]; then
  echo "::error::$BASELINE is missing. Generate it with: ./$SELF --update-baseline"
  exit 1
fi

declare -A EXPECTED=()
while IFS=$'\t' read -r term scope want; do
  case "$term" in ''|'#'*) continue ;; esac
  EXPECTED["$term/$scope"]="$want"
done <"$BASELINE"

fail=0
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${RATCHETED_SCOPES[@]}"; do
    key="$term/$scope"
    if ! got="$(count "$pattern" "$scope")"; then
      fail=1
      continue
    fi
    want="${EXPECTED[$key]:-}"
    if [ -z "$want" ]; then
      echo "::error::$BASELINE has no row for '$key'. Run --update-baseline."
      fail=1
    elif [ "$got" -gt "$want" ]; then
      echo "::error::$key rose from $want to $got occurrences — new '$term' vocabulary entered the tree (#1316 is retiring it)."
      fail=1
    elif [ "$got" -lt "$want" ]; then
      echo "::error::$key fell from $want to $got. Tighten the ratchet: ./$SELF --update-baseline, then commit $BASELINE. A baseline left high re-permits every occurrence this change removed."
      fail=1
    fi
  done
done <<<"$TERMS"

echo "--- informational only (not gated; the legacy bundle is being deleted) ---"
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  for scope in "${INFO_SCOPES[@]}"; do
    if ! got="$(count "$pattern" "$scope")"; then
      fail=1
      continue
    fi
    printf '    %-14s %-6s %s\n' "$term" "$scope" "$got"
  done
done <<<"$TERMS"

if [ "$fail" -ne 0 ]; then
  echo "::error::#1316 S0 terminology ratchet failed. This is drift control over a stated baseline, not a proof that any slice is complete; see the header of $SELF."
  exit 1
fi

echo "OK: retiring vocabulary is at or below the #1316 baseline in every ratcheted scope."
