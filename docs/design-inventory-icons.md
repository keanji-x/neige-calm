# Inventory presentation

The user-approved preview is implemented in the existing frontend, preserving
the deployed light/dark palettes, page columns and action callbacks. Review tier
L2: the shared typography contract and both inventory projections change.

Issue: https://github.com/keanji-x/neige-calm/issues/2447

The coordinating owner approves the shared styles, icon and panel primitive
changes for this task. Section headings share the existing navigation-label
role (12px/16px, medium); auxiliary icons share `--glyph-sm`. Existing spacing
tokens define one 14px icon column and an 8px text gap. The single Icon registry uses pinned Lucide components and registered lightweight
provider paths on the same 24-unit grid, without runtime shape exceptions.
No duplicate size tokens
or runtime DOM decoration are introduced.

The Track view model owns module order, tool labels and anonymous-terminal
names. Terminal grouping belongs to the inventory projection. It must retain
every card/action, custom name and authoritative status; Tools group by type (Terminals, Agents and Other tools), while Tasks group
by progress. Tool rows carry their exact runtime status; explicit failure or
input attention expands their type group and appears in its count. The graphics library owns stroked
shapes, while the Track feature owns the kind-to-graphic mapping. Unknown kinds
remain named accessibly, and hidden text preserves the projection contract.

Acceptance checks cover desktop/mobile projection parity, keyboard and tooltip
names for icon-only kind actions, collapsed terminal access, custom titles,
non-terminal tools, error visibility, and shared heading/icon geometry in both
themes. Worker identity and permissions are unchanged; this change does not
deduplicate or hide cards based on inferred task relationships.

Run focused core/UI/inventory tests, required frontend lint/build/tests and text
gates. Review the frozen diff through two independent channels before final
browser screenshots. Screenshots use the production application and read-only
4140 API data; no injected presentation rules replace the implemented code.
