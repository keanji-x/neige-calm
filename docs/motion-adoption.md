# Component motion adoption (#2208)

The user delegates visual judgement and component-by-component implementation, review and squash merge. Shared motion contracts come from #2200. Migrate existing interactions in coherent batches, keeping domain state and focus immediate. Do not add motion to static controls for cosmetic uniformity.

| Batch | Owning components | Contract |
| --- | --- | --- |
| Dialog | UI dialog and styles chrome | Entrance/backdrop recipes, readable text, immediate dismissal, focus/Escape/reopen/reduced motion |
| Disclosure | Shell, references, tasks, inventory groups | One short disclosure recipe; content and keyboard semantics stay with callers |
| Feedback/layout | Controls, rows, lists, headers, grid | Feedback and layout recipes; direct manipulation remains immediate |
| Progress/pointer | Context ring, edge navigation | Immediate values, predictable progress and pointer timing |
| Emphasis | Report arrival | One background highlight using a shared emphasis recipe |
| Native loops | Activity indicator, brand SVG | Shared declarations/opt-out contract, distinct owner-defined rhythms |

Styles owns duration/easing declarations and closed inventories; UI owns semantic recipe metadata and primitives. CSS handles small state feedback, SizeMotion handles intrinsic size with native Web Animations, and native SVG retains decorative geometry. Only add abstractions demanded by these consumers.

Review tier L1 per batch: presentation changes and declared token extensions, without authority or persistence changes. Run production-entry browser checks, relevant frontend/text gates, independent isolated review, then complete remote CI before squash merge. Record each merged PR in issue #2208 and close the issue only after all batches converge.

## Owner change request

The user explicitly authorized this work on 2026-10-06 without per-batch user review. The root orchestrator accepts narrow updates to styles duration/easing tokens, public token types/inventories, and existing global dialog/base/grid chrome. Frozen dialog/menu/focus behavioral interfaces remain unchanged. Owner trailers must name each changed frozen path and reference #2208. New runtime modules require prior registration.
