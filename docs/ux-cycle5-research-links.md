# UX cycle 5: follow saved research links

Outcome: clicking a table cell whose linkKey field contains a copied URL of a
Track in this application opens that Track rather than encoding the complete
URL as its id. Saved data is read unchanged. This is a generic Report table
behavior, with no portfolio renderer, migration, permissions or external browsing.

Core resolves legacy bare ids and the existing neige://wave/id#block protocol.
URL-looking inputs never fall back to bare ids. Browser navigation supplies the
URL parser with the current origin and configured app base path, checks same
origin and exact Track route shape, then reuses the existing report-link and
navigation helpers. Core receives that narrow app-route resolver as a dependency;
it reads no browser globals and does not grow another URL parser. Credentials,
other origins/ports, unknown schemes and non-Track paths do not become buttons.
Relative URL support is limited to root-relative routes under the supplied app
base, e.g. `/next/track/id`; page-relative paths and protocol-relative URLs are
refused. View query parameters are not carried into the report destination.
Valid ids are decoded once. Existing wave-link fragment compatibility is retained.

ReportDocument carries the optional resolver to native layout tables. Its saved
Recipe preview mode clears both the resolver and navigation callback. Original
bare-id tables remain usable without a browser resolver.

Acceptance starts with a real router + native table click using a copied same-
origin URL. It must request the destination id, render the research report and
preserve history Back. Focused parser tests cover same-origin/route/decoding
boundaries; browser tests retain labels, read-only preview and unchanged saved
content. No Rust or persisted schema changes, API generation or shared Cargo
cache are required. Two independent reviews precede root's real saved-URL GUI
trial and deployment.
