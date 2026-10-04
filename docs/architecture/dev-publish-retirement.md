# Retire development review events and name PR publication by its owner

Outcome: remove the retired review event and observation contracts; expose PR
publication as `neige.dev.publish` under the existing `neige.<object>.<action>`
grammar; move the shared forge payload constructor out of Worker tool handlers.
Task outcome contracts and publication execution/idempotency remain unchanged.

Review tier: L2, because removing a persisted event contract affects replay.

Production 4140 audit: 36 historical `review.round` rows, no review observations
in session snapshots, 12 `calm.track.publish` transcript tool fields, and no
stored recipes using either publication name. Preserve historical event rows
and raw cursor accounting; typed readers explicitly ignore the retired kind.
After migration 0134 normalizes old tool names, migration 0137 rewrites only
`$.item.tool` from `neige.track.publish` to `neige.dev.publish`. No tool alias.

Keep the durable `track.publish:` operation key unchanged so publication
retries still replay existing operations. PR scripts belong to development;
the transport owns the neutral forge payload type and constructor.

Acceptance: no review types in Rust, generated wire, Zod, or dispatch; retired
rows remain intact while typed readers and recovery continue past them; public
publish discovery/calls use the new name and retain plugin/role fences; publish
and candidate delivery share one neutral forge payload constructor. Run focused
Rust and frontend contract tests, regenerate wire types, and run text gates.
Consistency checks include the head-schema migration inventory, event kind and
golden inventories, the exact legacy-fixture insert census, and the retired-tool
scanner. A migration change must run `head_schema_fixture` locally as well as
its data rewrite test.
