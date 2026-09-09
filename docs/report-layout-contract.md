# Report layout contract change

Status: implementation decision by the root orchestrator, 2026-09-09.
Extends the frozen Report kind vocabulary; preserves existing kinds, Recipe
storage, current-Track source resolution and Report revision/authority rules.
User outcome: Recipe bodies compose native chart/table primitives, and normal
chat edits the persisted layout. No portfolio-specific renderer or transport.

## New `layout` block payload, version 1

All objects reject unknown fields. No nullable optional fields; omit absent
options. Text is bounded to 2048 Unicode code points. Numbers must be finite.

Root: `{version:1, columns:1|2|3, gap:"compact"|"normal"|"wide",
surface:"plain"|"muted", items:[item;1..12]}`.
Each item has required `kind:"chart"|"table"`, `title:string`, `span:1|2|3`
(must not exceed root columns), and `data:Data`. Optional `exclude:Selector`.
Selector: `{key:nonempty string,value:string|number|null}`; exact scalar equality.

Data is exactly one of `{source:live-source, annotations?:Annotations}` and
`{rows:[Row;0..500]}`. Live source uses the existing
`neige://plugin/<plugin_id>/<overlay_kind>` syntax, never a Track ID or URL.
Row is an object with 0..32 nonempty keys and scalar string/number/null values.
Annotations: `{keys:[nonempty string;1..4],rows:[Row;0..500]}`. Keys unique;
every annotation row must have all join keys as non-null scalars; join tuples
unique. Annotation values must not overwrite a producer field except identical
join keys. Invalid/ambiguous joins produce a visible data error at rendering.

Chart item adds required `chart:"line"|"donut"`, `x:nonempty string`,
`y:nonempty string`, `height:integer 160..640`, `color:"#RRGGBB"`;
optional `unit:Unit`, `ranges:[integer days 1..3660;1..8]`,
`defaultRange:integer days 1..3660`. Ranges unique, ascending; defaultRange must
be present iff ranges is present and must occur in it; only line accepts ranges.
Unit: `{key:nonempty string,equals:nonempty string,row?:Selector}`.
Without row, each observation must match unit.equals: mismatches are gaps in a
line and prevent a donut. With row, exactly one source row must match selector
and its unit key must equal unit.equals, or the whole chart is unavailable.
Evaluate unit.row before exclude. Unit.equals supplies the displayed unit.
The template chooses a settlement currency; a changed plugin currency shows
unavailable until the saved configuration is updated, never relabels values.

Line x accepts finite epoch milliseconds or parseable date strings. Sort stably,
including duplicate timestamps. Invalid/missing y stays a gap; invalid x makes
the dataset visibly invalid. Zero is data. Render a line only with >=2 usable
observations. Ranges are trailing calendar-independent days from latest x.
Donut x is a non-null scalar label; y must be finite and nonnegative in EVERY
selected row. A missing/negative y prevents normalization of the whole chart.
Exclude summary rows through explicit template selectors. Preserve producer
caption separately from template title. Empty/all-zero data stays empty.

Table item adds required `columns:[Column;1..32]`, optional `total:Total`.
Column: `{key:nonempty string,label:string,format:"text"|"number"|"percent"|"share",
digits:integer 0..8, fallbackKey?:nonempty string, suffixKey?:nonempty string,
linkKey?:nonempty string}`. Column keys unique. Missing values display a dash.
Percent displays the supplied percentage (does not multiply by 100).
Share divides the numeric column value by the explicitly selected total *100;
it is unavailable if any selected row's value is missing/negative, or the total
is missing/nonpositive, or selected values fail to sum to it within independent
cent rounding. `total` required iff any column uses share.
Total: `{row:Selector,key:nonempty string}`; select exactly one source row before
exclude. linkKey names a row field containing a Track ID, opened only through
the native Report callback. It is not an arbitrary URL. Numeric suffix values
are display-only labels (e.g. native price currency), not conversion rules.

## Composition and migration

The portfolio Recipe uses a two-column layout with line and donut, then a
one-column layout containing an enriched holdings table, then an inline table
layout for the transaction log. Headings and ordering remain ordinary Report
body/blocks. Each component's configuration belongs to the Recipe/Report.
Annotations hold optional research Track IDs, display names and next events;
empty template annotations carry no investor data. Trade rows start empty.
No migration, no special portfolio route, no browser-selected portfolio ID.
Demo fixtures will exercise this same native renderer; live preview will use
normal app transport and persisted Reports, with no synthetic report projection.

## Validation and acceptance

Backend strict validator and MCP self-description must match frontend schema.
Validate via actual Recipe save/create and Report block write entry points,
including bad unknown fields, malformed source, incomplete ranges, oversized
layout, duplicate annotations, and span over columns. Existing released
migrations stay frozen. Existing old clients may visibly degrade unknown kind;
do not claim that they render it. Next-generation frontend is the target.
Test current-Track source isolation, persisted edit/reload, partial allocations,
currency gaps, duplicate timestamps, producer-caption retention and clean
template instantiation. Run focused gates and two fresh independent reviews.
