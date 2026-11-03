# Native notification backpressure (#2986)

Review tier: L2, because native ingress feeds persisted conversation content.

The Codex harness must receive every notification published during its live
subscription, in order, even while it waits for the SQLite writer or a held
approval write. Increasing the broadcast ring does not establish that contract.

Keep the existing lossy broadcast for diagnostic and liveness observers. Add a
bounded, lossless subscription for harness consumers at the shared daemon owner.
The forwarder reserves space before publishing, outside native-source locks and
before taking completion bookkeeping locks. The final synchronous publication
still uses the existing current-source fence. Dropping a harness receiver releases
backpressure; subscriptions do not spawn background tasks or retain a harness.

A slow harness can delay daemon notification forwarding. RPC replies still use
the provider transport's independent reader and existing unbounded notification
handoff, so RPC responses must remain readable while forwarding is blocked.
This does not add crash durability or change the pre-existing transport buffer;
it removes the bounded ring's silent eviction between transport and persistence.

Acceptance checks: reproduce an intervening item lost behind a held SQL writer;
verify the item and its presentation persist after release; saturate the bounded
subscription through the real WebSocket path and verify an RPC response remains
readable; verify ordering, receiver cancellation/drop, and the existing source
replacement fence. Mutation verification skips a full subscriber instead of waiting, and must make
the bounded-handoff no-loss test fail.

Codex harness subscriptions no longer report `Lagged`. The old Codex-only
broadcast-overflow integration test is superseded by the real-transport held-writer
regression. The generic lost-input unit contract remains for other providers'
broadcast streams, including suppression of partial replies after input loss.
