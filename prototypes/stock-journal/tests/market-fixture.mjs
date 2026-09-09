export function marketFixture(trackId = 'market-track', now = Date.now()) {
  const holdings = {
    columns: [
      { key: 'asset', label: 'Asset' }, { key: 'venue', label: 'Venue' },
      { key: 'qty', label: 'Quantity', align: 'right' }, { key: 'price', label: 'Price', align: 'right' },
      { key: 'currency', label: 'Priced in' }, { key: 'rate', label: 'Rate to CNY', align: 'right' },
      { key: 'value', label: 'Value (CNY)', align: 'right' },
    ],
    rows: [
      { asset: 'AAA', venue: 'US', qty: 10, price: 10, currency: 'USD', rate: 7, value: 700 },
      { asset: '00005', venue: 'HK', qty: 10, price: 20, currency: 'HKD', rate: .9, value: 180 },
      { asset: 'Total', venue: null, qty: null, price: null, currency: 'CNY', rate: null, value: 880 },
    ],
    caption: 'Priced at 2026-09-09T00:00:00Z, totalled in CNY. Converted at USD → CNY 7; HKD → CNY 0.9',
    highlight: 'Total',
  };
  const history = {
    columns: [{ key: 'at', label: 'At' }, { key: 'total', label: 'Total', align: 'right' }, { key: 'currency', label: 'Currency' }, { key: 'change', label: 'Change', align: 'right' }],
    rows: [
      { at: '2026-09-09T00:00:00Z', total: 880, currency: 'CNY', change: 30 },
      { at: '2026-08-09T00:00:00Z', total: 850, currency: 'CNY', change: null },
      { at: '2026-07-09T00:00:00Z', total: 100, currency: 'USD', change: null },
      { at: '2026-06-09T00:00:00Z', total: 800, currency: 'CNY', change: null },
      { at: '2026-05-09T00:00:00Z', total: 90, currency: null, change: null },
    ], caption: 'Total portfolio value over time, newest first',
  };
  const overlays = [holdings, history].map((payload, index) => ({ id: `overlay-${index}`, plugin_id: 'dev-neige-market', entity_kind: 'track', entity_id: trackId,
    kind: index === 0 ? 'portfolio.holdings' : 'portfolio.history', payload, updated_at: now }));
  return { holdings, history, overlays };
}
