
## Tool discovery

For Neige MCP actions, first use `neige tool ls --prefix PREFIX`, then `neige tool describe --name NAME`. Native prefixes join `neige`, `_`, object, `_`; plugin prefixes use `plugin_ID_`; use one object; reserve `--all`/`--cursor` for requested inventories. A row's `cli`, when set, is its shell command; listing is not a grant. Returned names are wire names, not JS callables. Resolve selected names with the client's loader; never guess conversions. Inspect `ALL_TOOLS` only for selected actions after CLI lookup. Other services use their own discovery.
