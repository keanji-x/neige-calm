# CR-1501-FE: independent task entry

Parent orchestrator approved these narrow additions for #1501's single closed
loop in the implementation assignment. HTTP contracts remain those in
`docs/design-1501-independent-task-entry.md`.

- `core/domain/independent-task.ts` owns named start/report operations and Zod
  schemas, immutable intent types, revision/declaration reads and uncertainty
  classification. It uses the existing `ApiTransportPort` without widening it.
- `features/track/page/public.tsx` adds optional `onCreateTask`; app supplies the
  goal dialog through the existing Track route. The form only renders props.
- `features/report/task/recovery.tsx` adds optional `renderReport(attemptId)`;
  app supplies the exact-attempt read beneath current execution and older
  attempts. Recovery/history behavior and ownership remain unchanged.
- `app/router` mints a nonce once per submitted intent and caches the immutable
  request in the session QueryClient. Synchronous cache state fences double
  activation and survives route unmounts. Unknown writes reconcile against the
  authored declaration, or retry unchanged. A rejected retry cannot disprove a
  previous uncertain commit. Only a new explicit submission after a definite
  rejection can create a new intent. No reload-persistent task store is added.
- Query invalidation uses the existing Track detail/report prefixes. Accepted
  report identity includes Track, task key and attempt. Model result JSON and
  reported artifact strings render as text, without downloads or HTML execution.

All additions are inside existing non-readonly directory owners; no ownership
inventory, global style, event protocol or generic transport changes are needed.
Parent owns wire/OpenAPI generation and final integrated verification/reviews.
