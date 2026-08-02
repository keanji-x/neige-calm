# Frontend architecture

## Layer overview

Runtime dependencies flow one way: `app → features → systems → ui → core`. A layer may skip downward, but never import upward; feature domains never import one another. `styles` is an owned non-runtime layer.

## Placement

Cross-platform, platform-independent logic belongs in `core`; browser composition and behavior live below `web/src` in `app`, `features`, `systems`, and `ui`. `web/src` may contain only `main.tsx` at its root. Do not add `shared` or barrel files.

## Change requests

Frozen interfaces and global style contracts are read-only during implementation. If an interface is insufficient, stop and submit a change request to its owning layer for orchestrator decision and broadcast; agents must not fork or silently widen the contract.

## Verification

Every architecture gate needs a positive and negative fixture. Keep checks static and explicit; do not weaken a rule or add an allowlist without a narrow path and documented reason.
