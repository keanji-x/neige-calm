# Formatted captured-source reading

## Outcome and acceptance

Captured source panels currently print Markdown syntax in a raw preformatted body.
Provide a formatted reading view by default and an exact original-text view.
Full source readers receiving a quote anchor still open original text and highlight the existing
first exact occurrence and scrolls only when the host allows it. Provenance wording,
metadata, missing/error states and stored bytes remain unchanged.

Both hover previews and drawers use ReportSourcePanel, so the fix belongs there.
Reuse the report's sanitized Markdown renderer with an explicit inert destination
mode: source-owned links and images produce readable labels without navigation,
previews or remote resource loads. HTML remains dropped. Original text is always
available, including content omitted from the reading view.

## Ownership and design

Split source citation controls from source panel composition to remove the import
cycle before the panel consumes report/document/content. No new Markdown renderer
or parser policy is introduced. Document rendering retains interactive destinations;
captured evidence selects the inert mode explicitly. Quote offsets remain owned by
sourceHighlight and are not projected onto normalized rendered text.

Use reading/original controls with explicit pressed states. Hover previews explicitly start in reading mode even for quote citations. Full-reader
quote-bearing arrivals start in original mode; changing source or anchor resets the mode. All provenance
variants use the same view policy, with no plugin-specific identity assumptions.

L2: this adds formatted presentation at an untrusted captured-evidence boundary and
changes composition among shared report consumers. Two isolated independent full
reviews must converge. No schema, migration, persisted content or navigation-policy
change is needed.

## Verification

First add a failing production-panel reproduction for Markdown headings/strong/list
and exact original-view recovery. Test inert destinations, HTML/images, quote-mode
arrival/reset and the existing first-occurrence/highlight semantics. Mutation-verify
the inert destination fence in an exclusive checkout using complete observing test
targets. Run focused tests, affected real-browser paths, lint/build/unit checks,
repository text gates and integrated Tier 1 checks. CI owns broader suites.
