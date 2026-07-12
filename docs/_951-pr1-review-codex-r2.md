# PR1 final re-review — issue #951

## VERDICT

**ship**

Both prior findings are resolved. I found **0 BLOCKER, 0 MAJOR, and 1 MINOR** new finding. The minor is a test-assertion gap, not evidence of a production bug.

## Findings

### MINOR — Ensure tests do not directly assert all payload/start-request invariants

**Evidence:** `crates/calm-server/tests/cases/today_launchpad.rs:125-132,136-169,194-217`

The new tests cover the most important durable behavior: repeated ensure returns 200, preserves all returned IDs and a persisted `harness_items` row, and leaves one marked wave; adoption returns 201, deletes the legacy harness item, keeps the IDs/terminal, and restores the marker.

Two assertions are weaker than their test names/contracts imply:

- The “idle spec” check inspects the initial card payload for an absent `harness.goal`. That payload is built by `spec_payload()` and is separate from `SpecHarnessStartOperationPayload.goal`, so it would still pass if the operation request stopped using `goal: None`.
- The idempotency test does not seed and then compare a distinctive spec-card payload, while the adoption test seeds a legacy payload but never asserts that it was reset to the fresh snapshot. Thus payload preservation-on-reuse and payload reset-on-adoption are not directly regression-protected.

**Fix:** inspect the persisted/submitted operation payload and assert `goal == null`, `reset_harness_items`, and `force_new_thread` for fresh/reuse/adoption as appropriate. Seed a distinctive payload before repeated ensure and assert byte/JSON equality afterward; after adoption, assert `snapshotVersion == 0` and an empty `pendingQueue`.

## Re-review conclusions

- **Prior MAJOR resolved:** `today_launchpad_ensure_tx` now separates `created` from `adopted_legacy`. Existing marked launchpads take `(false, false)`, reuse the spec card without deleting `harness_items` or replacing its payload, and return 200 after successful harness reuse. Legacy adoption takes `(false, true)`, resets only the adopted spec transcript/payload, preserves a linked terminal, and returns 201. Fresh insertion takes `(true, false)`, creates the required cards, and returns 201. Both `reset_harness_items` and `force_new_thread` are exactly `created || adopted_legacy`. I found no residual successful path that erases an existing marked launchpad transcript.
- **Prior MINOR resolved:** `is_unique_constraint` requires a database unique violation whose SQLite message names the expected partial index (`idx_coves_one_system` or `idx_waves_one_launchpad`). Non-unique and unrelated database errors propagate immediately. The launchpad retry uses `.await?`, so a retry failure returns the retry's actual error instead of replacing it with the first error.
- **WaveRow audit clean:** every `query_as::<_, WaveRow>` found in the repository selects `purpose`, including both reads in `wave_vcs/snapshot.rs` and `wave_lifecycle.rs`. I found no remaining full-WaveRow projection mismatch.
- **Golden files consistent:** `Wave.purpose` uses `#[serde(default)]` without `skip_serializing_if`, matching `workflow_id`; old minimal input may omit it, while canonical/minimal and full serialization correctly emit `"purpose": null`.
- **Holistic pass:** the singleton/adoption/card/terminal/status flow remains internally consistent. No additional production correctness, security, or data-loss issue emerged from the complete diff.

## Review constraints

Static review only, as requested. I did not build or run tests and made no code changes outside this report.
