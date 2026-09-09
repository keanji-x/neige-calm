# Framework

Composition belongs to the saved Recipe/Report, not this preview.
See [the native layout contract](../../docs/report-layout-contract.md).
The reusable chart primitive is `fe/web/src/ui/chart/public.tsx`; the Report
layout renderer resolves only declared sources in the current Track and passes
validated observations to it. Tables and chart placement are configured by the
Recipe. Prototype-specific iframe resources, quote imports and Report projections
have been removed.
