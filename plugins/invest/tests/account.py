"""An in-process simulated paper account for long or wide histories: every market order fills at the
quoted price at once. The production Portfolio still validates, reconciles and sizes everything."""
from copy import deepcopy
from decimal import Decimal


class SimulatedAccount:
    def __init__(self, cash, prices, clock, split=False):
        self.cash, self.split = Decimal(cash), split  # split: one fill per share
        self.prices = {symbol: Decimal(price) for symbol, price in prices.items()}
        self.positions, self.orders, self.fills, self.clock = {}, [], [], clock

    def snapshot(self, since, symbols):
        quoted = (set(symbols) | set(self.positions)) & set(self.prices)
        return deepcopy({
            'identity': {'account_no': 'PAPER123', 'account_channel': 'lb_papertrading'},
            'cash_usd': str(self.cash), 'available_cash_usd': str(self.cash),
            'positions': {s: {'shares': n, 'available_shares': n} for s, n in self.positions.items()},
            'quotes': {s: {'price': str(self.prices[s]), 'at': self.clock().isoformat(), 'status': 'Normal'}
                       for s in sorted(quoted)},
            'market_open': True, 'orders': self.orders, 'fills': self.fills})

    def submit(self, request):
        order_id = f'sim-{len(self.orders) + 1}'
        symbol, quantity, price = request['symbol'], request['quantity'], self.prices[request['symbol']]
        sign = 1 if request['side'] == 'Buy' else -1
        self.cash -= sign * quantity * price
        self.positions[symbol] = self.positions.get(symbol, 0) + sign * quantity
        if not self.positions[symbol]:
            del self.positions[symbol]
        self.orders.append({key: request[key] for key in ('symbol', 'side', 'order_type', 'remark',
                                                          'time_in_force', 'outside_rth')}
                           | {'order_id': order_id, 'quantity': str(quantity),
                              'executed_quantity': str(quantity), 'status': 'Filled'})
        lots = [1] * quantity if self.split else [quantity]
        self.fills += [{'trade_id': f'{order_id}-fill-{k}', 'order_id': order_id, 'symbol': symbol,
                        'quantity': str(lot), 'price': str(price), 'time': self.clock().isoformat()}
                       for k, lot in enumerate(lots)]
        return order_id
