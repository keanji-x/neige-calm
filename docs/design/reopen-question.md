# Reopen a closed track by answering the Planner

Current-main implementation for #2410. Reuse `neige_user_ask`, the existing tagged
answers and question drawer. A Planner asks one title with `action: "reopen_track"`;
the ask owner supplies Reopen and continue / Keep closed choices; the lifecycle
question drawer offers these choices without a custom-answer field. Only clicking
option 0 authorizes restoring the track; typed words remain text. Ordinary and
provider-native wake/hold asks retain their contracts.

L2: the user answer changes persistent, user-controlled lifecycle state. Store
an optional typed AskAction carrying the exact closure stamp on AskRequested;
absence preserves released events. Bind the answer using the existing ask id,
track scope and open-ask predicate. In the same user-authored transaction, check
current closure and intervening reopen events (including timestamp collisions),
reuse the lifecycle owner's eligibility fence, update the track and append
AskAnswered. Denials/text append only AskAnswered. The existing dispatcher wakes
the Planner from that answer.

Lifecycle asks remain visible until answered, regardless of later messages or
notification dismissal. Explicit action metadata crosses the activity contract;
the track feature omits generic Dismiss. The existing question UI owns its state.
Lifecycle refusals use 400: existing 409 means the ask is terminal and the drawer
settles it. Keep request/grant eligibility in the lifecycle owner and action
interpretation/canonical choices in the ask owner.

Acceptance: no restore before consent, on denial or on typed matching labels;
correct user actor and atomic events; no repeated/cross-track/stale grant;
changed closure and child restrictions; clarification/dismissal cannot hide the
question; native/hold asks cannot gain effects by matching words; desktop/phone
choices use the current answer endpoint. Approved scoped contract change request
for core/api and action metadata in core/domain and track feature.

Verification plan: focused `api_suite` user_ask tests and frontend ask/route tests;
mutation-verify grant tagging and closure fence in the exclusive worktree; real
API/wire and registry/prompt generators; text/contract gates and quick Rust
preflight; frontend lint/build/tests, focused browser paths and Tier 1 stack E2E.
Two independent fresh L2 reviews follow fixes. Broad Rust suites remain CI-owned.
