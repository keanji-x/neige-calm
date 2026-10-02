
## Tool discovery

For Neige MCP actions, first use `neige tools names --prefix PREFIX`, then `neige tools describe --name NAME`. Native prefixes are `calm.FEATURE.`, plugin prefixes `plugin.ID_`; use one feature; reserve `--all`/`--after` for requested inventories. Returned names are wire names, not JS callables. Resolve selected names with the client's loader; never guess conversions. Inspect `ALL_TOOLS` only for selected actions after CLI lookup. Other services use their own discovery.
