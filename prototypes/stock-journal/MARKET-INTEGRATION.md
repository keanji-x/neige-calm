# Market data integration

Use the current `dev-neige-market` producer on main, not a second price provider.
Read the selected Track's `portfolio.holdings` and `portfolio.history` overlays
through the existing authenticated Track-detail endpoint. A read-only transport
projection supplies the portfolio Report without overwriting the saved Report.

Keep producer-converted holding values authoritative, preserve its currency and
conversion disclosures, and never turn a partial total into complete weights.
History rows in another/unknown currency become gaps, not points relabeled into
the current currency. Latest main does not publish daily stock changes in these
overlays; that column stays unavailable unless the producer supplies it.

Live mode uses Neige's native login/session gate and the configured local backend.
No tokens are read from files or passed to the chart iframe. Only the selected
Track's validated chart snapshot crosses a one-way, source-checked message bridge.
The iframe remains opaque-origin; it receives no API or write capability.

Research Track mappings, next events, and executed trade records are read from
`.neige-portfolio/metadata.json` through the selected Track's existing authenticated
workspace-file endpoint. They must never be embedded into public frontend assets.
Only the file route's exact missing-path 400 means no metadata yet; a 404 means
the Track is gone. Denied, truncated or invalid reads remain errors.
Missing metadata stays empty. This does not place trades or
invent transaction history from changes in holding quantities.

Acceptance: exact plugin/Track overlay matching; partial values stay incomplete;
unknown-currency history remains gaps; live read errors retain error states; iframe
messages cannot select another Track; native login is required for private reads;
saved backend reports are not changed; actual plugin-shaped fixtures and the
built browser flow both pass. Current demo and static styling remain available.
