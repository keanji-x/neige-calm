# Portfolio framework outcome

The existing Neige Report hosts three sections: portfolio charts, holdings with
research Track links and next events, and executed-trade history. No factor model
or broker order execution is included.

Demo mode uses one validated snapshot. Live mode reads the selected Track's
`dev-neige-market` holdings/history overlays via Neige's authenticated API. Plugin
converted values and totals are authoritative. Missing values withhold complete
weights; differing or unknown history currencies produce explicit gaps.

Research links, events and trades come from `.neige-portfolio/metadata.json` in
the portfolio Track workspace, via the existing authorized file reader. They are
not public build assets. This view does not overwrite the backend's saved Report,
and other research Tracks keep their original Reports.

Acceptance: charts and tables agree on producer values; source/Track boundaries
are checked; authentication gates private reads; metadata failures never become
empty records silently; links preserve research; built HTML/CSS works in the
existing sandbox. See MARKET-INTEGRATION.md and README.md for the data contract.
