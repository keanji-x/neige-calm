
## Tool discovery

For Neige MCP actions, first use `neige tool list --prefix PREFIX`, then `neige tool describe --name NAME`. Native prefixes join `neige`, `.`, object, `.`; plugin prefixes use `plugin.ID_`; use one object; reserve `--all`/`--after` for requested inventories. A row's `cli`, when set, is its shell command; listing is not a grant. Returned names are wire names, not JS callables. Resolve selected names with the client's loader; never guess conversions. Inspect `ALL_TOOLS` only for selected actions after CLI lookup. Other services use their own discovery.
