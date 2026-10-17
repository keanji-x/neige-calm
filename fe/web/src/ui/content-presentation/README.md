# Content admission

`useContentAdmission(selectedKey: string | null, eligible: boolean): boolean` gives
one eligible selection a shell rendering opportunity before permitting its content
to mount. It owns only visit-scoped animation-frame callbacks and their cancellation.
It has no Query, Router, feature, preparation, cache or motion dependency.

A non-null eligible selection starts a fresh visit. Two rendering callbacks and a
React transition admit it; these callbacks do not prove two painted frames or
animation completion. Null selection, lost eligibility, a different selection or
unmount cancels pending callbacks. A→B→A is a new visit, so a late callback from the
first A cannot admit the second A. Once admitted, same-selection updates remain
immediate. Independent hook instances own independent visits.

The return value is **permission to mount**, not evidence of actual content commit,
visibility, successful data loading, read eligibility or authorization. The consumer
supplies its existing eligibility and keeps queries/commands in their original
owners. A content subtree supplies any actual commit evidence its caller needs.
Do not perform preparation inside a transition callback or wait for this hook to
start a data request. Current content callbacks and live business data remain with
the consumer; the hook does not create a deferred data snapshot.

New consumers pass their stable selected key and existing content eligibility, then
render their content when permitted. They compose their own pending/error surface
and use the existing UI motion owner for animation. There is no central registry,
per-feature delay, extra provider or test-name inventory to edit.

`public.test.tsx` pins visit lifecycle and cancellation. Router conversation tests
exercise the production consumer, source-dependent actions and receipt ordering.
Timing evidence belongs in the relevant pull request; this contract makes no paint,
frame-rate, total loading time or interruptible DOM-commit guarantee.
