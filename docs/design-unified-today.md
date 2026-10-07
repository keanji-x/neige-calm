# Unified Today

The homepage resolves the daily Planner Track and composes its existing report, cards, tasks and conversations with the existing Calendar feature. There is one Today route implementation. `/today/legacy` redirects to the homepage, preserving a historical `day`; it does not read or mutate the earlier singleton launchpad. Backend APIs and persisted reports remain untouched.

Calendar appears before the daily report in the document column. Conversations retain the shared full-height drawer in the independent trailing column. This removes the competing vertical allocation between a month calendar and multiple conversation panes, rather than adding a second special conversation host. Compact pages use the same leading content and existing report/conversation navigation.

Calendar date selection remains local to the agenda and does not switch the daily report. Historical pages seed the agenda from the report date. Plugin failures retain the existing calendar adapter's feedback and date surface. App owns cross-feature composition/navigation; Calendar owns scheduling; Track consumes a generic leading-content slot; the existing conversation lifecycle and drawer remain authoritative.

The shared ChatComposer explicitly grows its editor to three visible rows, then scrolls the draft internally. Text, send/edit semantics and recovery state remain unchanged. This preserves space for the card header, actions and recovery strips when generic drawers share their height with a companion, including small desktop windows.

Acceptance uses the production router and the production `#root` height contract: Today and the legacy redirect share one implementation; Calendar navigation and Week/Month stay usable while conversations are open; populated transcripts, multiline drafts, both companion inputs and growing recovery strips remain inside their cards down to 600px; report evidence, sources and compact navigation retain their existing paths.

Review tier L2 because retiring a complete obsolete frontend implementation creates a large diff. No backend, authorization, storage, migration or deployment-configuration changes. Both independent channels re-review the complete candidate after behavioral fixes.
