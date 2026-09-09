# Market data integration

The Recipe names the existing `dev-neige-market` `portfolio.holdings` and
`portfolio.history` overlays. Normal Neige Track rendering resolves those sources
with its existing current-Track resolver and update infrastructure. A market
update changes displayed observations, never the Report revision or its layout.

The template explicitly excludes the Total row from allocation, checks its CNY
denominator, and leaves missing or foreign-currency historical observations as
gaps. Every allocation value must be present before normalization. Same-time
history observations retain their stable order. Producer captions remain visible,
including on data errors, so valuation timestamps and FX disclosures are retained.

Optional display names, research Track IDs and events are annotations in the
saved holdings component. Trade records are inline rows in the saved log
component. There is no private metadata file, browser-selected Track, second
market provider, synthetic Report, or chart iframe communication bridge.
