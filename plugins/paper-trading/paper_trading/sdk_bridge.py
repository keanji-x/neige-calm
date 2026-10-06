"""Isolated official SDK entry point. No CLI confirmation-code redemption."""
import argparse
from datetime import datetime, time, timezone
from decimal import Decimal
import json
from pathlib import Path
import sys
import urllib.request
from urllib.parse import urlparse
from zoneinfo import ZoneInfo

# Direct -I entry point: load only this installed App's source.
sys.path.insert(0, str(Path(__file__).parents[1]))
from paper_trading.config import exact, identifier, integer, money, timestamp


ACCESS_POINTS = {'global': 'longbridge.com', 'cn': 'longbridge.cn'}
NY = ZoneInfo('America/New_York')


class OrderNotSubmitted(ValueError):
    """A completed local preflight; no broker write was invoked."""


def enum(value):
    return str(value).rsplit('.', 1)[-1]


def utc(value):
    # Official SDK 5.2.0 PyOffsetDateTimeWrapper uses from_timestamp(epoch, None):
    # its naive values are LOCAL time, including fold on DST transitions.
    # astimezone preserves that epoch; user-supplied timestamps still require TZ.
    return value.astimezone(timezone.utc).isoformat()


def contexts(client_id, interactive=False, access_region="global"):
    from longbridge.openapi import AssetContext, Config, OAuthBuilder, QuoteContext, TradeContext

    def authorize(url):
        if not interactive:
            raise ValueError('SDK OAuth authorization required; run the documented login command')
        print('Open this official authorization URL to authorize the PAPER account:', url, flush=True)

    if access_region not in ACCESS_POINTS:
        raise ValueError("unknown official access region")
    host = ACCESS_POINTS[access_region]
    oauth = OAuthBuilder(client_id).build(authorize)
    config = Config.from_oauth(oauth, http_url="https://openapi." + host,
        quote_ws_url="wss://openapi-quote." + host + "/v2",
        trade_ws_url="wss://openapi-trade." + host + "/v2",
        enable_papertrading=True, enable_overnight=False, enable_print_quote_packages=False)
    return AssetContext(config), TradeContext(config), QuoteContext(config)


def identity(asset, trade, expected):
    from longbridge.openapi import StatementType
    channels = trade.stock_positions().channels
    if not channels or any(c.account_channel != 'lb_papertrading' for c in channels):
        raise ValueError('SDK requires the official paper-account channel')
    statements = asset.statements(StatementType.Daily, start_date=1, limit=1).list
    if not statements:
        raise ValueError('SDK account proof unavailable: a daily statement is required')
    url = asset.statement_download_url(statements[0].file_key).url
    parsed = urlparse(url)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError('statement download must use HTTPS')
    # Signed broker document URL: no OAuth header is forwarded to the download host.
    with urllib.request.urlopen(url, timeout=10) as response:
        if urlparse(response.url).scheme != 'https':
            raise ValueError('statement download redirected to insecure transport')
        data = response.read(10_000_001)
    if len(data) > 10_000_000:
        raise ValueError('statement proof exceeds size budget')
    number = json.loads(data)['MemberInfo']['AccountNo']
    if number != expected:
        raise ValueError('SDK account does not match the configured paper account')
    return {'account_no': number, 'account_channel': 'lb_papertrading'}


def position(trade):
    positions = [p for c in trade.stock_positions().channels for p in c.positions]
    if any(p.symbol != 'SPY.US' or p.currency != 'USD' or p.quantity < 0 for p in positions):
        raise ValueError('SPY profile found an unsupported position')
    if len(positions) > 1:
        raise ValueError('duplicate SPY positions')
    funds = trade.fund_positions()
    if any(c.positions for c in funds.channels):
        raise ValueError('SPY profile does not own fund positions')
    return (integer(positions[0].quantity, zero=True), integer(positions[0].available_quantity, zero=True)) if positions else (0, 0)


def cash(trade):
    balances = trade.account_balance(currency='USD')
    if len(balances) != 1 or balances[0].currency != 'USD':
        raise ValueError('exactly one USD account balance required')
    balance = balances[0]
    infos = balance.cash_infos
    if any(i.currency != 'USD' and any(getattr(i, k) != 0 for k in
            ('available_cash', 'frozen_cash', 'settling_cash', 'withdraw_cash')) for i in infos):
        raise ValueError('SPY/cash profile found non-USD cash')
    usd = [i for i in infos if i.currency == 'USD']
    if len(usd) != 1:
        raise ValueError('USD available cash required')
    # Never use margin buying power as cash. Unsettled sale proceeds cannot fund a buy.
    total = Decimal(balance.total_cash)
    available = min(total, Decimal(usd[0].available_cash), Decimal(balance.buy_power))
    available -= max(Decimal(0), Decimal(usd[0].settling_cash))
    return str(total), str(max(Decimal(0), available))


def market(quote, now=None):
    from longbridge.openapi import Market
    rows = quote.quote(['SPY.US'])
    if len(rows) != 1 or rows[0].symbol != 'SPY.US':
        raise ValueError('exactly one SPY quote required')
    row = rows[0]
    local = (now or datetime.now(timezone.utc)).astimezone(NY)
    queried_date = local.date()
    # A failed calendar read raises: the whole snapshot fails rather than guess the trading day.
    days = quote.trading_days(Market.US, queried_date, queried_date)
    half_day = queried_date in days.half_trading_days
    trading_day = half_day or queried_date in days.trading_days
    local = (now or datetime.now(timezone.utc)).astimezone(NY)
    closing = time(13) if half_day else time(16)
    is_open = local.date() == queried_date and trading_day and time(9, 30) <= local.time().replace(tzinfo=None) < closing
    return {'price': str(row.last_done), 'at': utc(row.timestamp), 'status': enum(row.trade_status),
            # The calendar's own date: the caller's later clock may already be past midnight.
            'calendar_date': queried_date.isoformat(), 'trading_day': trading_day, 'half_day': half_day,
            'regular_open_at': datetime.combine(queried_date, time(9, 30), tzinfo=NY).astimezone(timezone.utc).isoformat(),
            'regular_close_at': datetime.combine(queried_date, closing, tzinfo=NY).astimezone(timezone.utc).isoformat()}, is_open


def order(row):
    return {'order_id': row.order_id, 'symbol': row.symbol, 'side': enum(row.side),
            'order_type': enum(row.order_type), 'quantity': str(row.quantity),
            'executed_quantity': str(row.executed_quantity), 'status': enum(row.status),
            'remark': row.remark, 'time_in_force': enum(row.time_in_force),
            'outside_rth': 'RTH_ONLY' if enum(row.outside_rth) == 'RTHOnly'
                           else (enum(row.outside_rth) if row.outside_rth is not None else None)}


def snapshot(asset, trade, quote, expected, request):
    exact(request, {'since'})
    proof = identity(asset, trade, expected)
    since = timestamp(request['since']) if request['since'] is not None else None
    now = datetime.now(timezone.utc)
    today = trade.today_orders()
    history = trade.history_orders(start_at=since, end_at=now) if since else []
    # Deduplicate identities first, then read the authoritative detail for each.
    ids = {o.order_id for o in today + history}
    if len(ids) > 500:
        raise ValueError('SDK order history exceeds reconciliation budget')
    orders = [order(trade.order_detail(k)) for k in sorted(ids)]
    executions = trade.today_executions()
    if since:
        executions += trade.history_executions(start_at=since, end_at=now)
    if len(executions) > 5000:
        raise ValueError('SDK executions exceed reconciliation budget')
    fills = [{'trade_id': f.trade_id, 'order_id': f.order_id, 'symbol': f.symbol,
              'quantity': str(f.quantity), 'price': str(f.price), 'time': utc(f.trade_done_at)} for f in executions]
    shares, available = position(trade)
    cash_usd, cash_available = cash(trade)
    current, opened = market(quote)
    return {'identity': proof, 'cash_usd': cash_usd, 'available_cash_usd': cash_available,
            'shares': shares, 'available_shares': available, 'quote': current,
            'market_open': opened, 'orders': orders, 'fills': fills}


def prepare_submission(asset, trade, quote, expected, request, policy):
    from longbridge.openapi import OrderSide, OrderType, OutsideRTH, TimeInForceType
    exact(request, {'symbol', 'side', 'quantity', 'order_type', 'time_in_force', 'outside_rth',
                    'client_request_id', 'remark', 'basis_shares', 'not_after'})
    if request['symbol'] != 'SPY.US' or request['order_type'] != 'MO' or request['time_in_force'] != 'Day' or request['outside_rth'] != 'RTH_ONLY':
        raise ValueError('only regular-session SPY DAY market orders supported')
    if request['side'] not in ('Buy', 'Sell') or type(request['quantity']) is not int:
        raise ValueError('typed side and integer shares required')
    integer(request['quantity'])
    identifier(request['client_request_id'])
    if not request['remark'].startswith('nc-spy-') or len(request['remark']) != 39:
        raise ValueError('immutable allocation remark required')
    identity(asset, trade, expected)
    current, opened = market(quote)
    if not opened:
        raise ValueError('SPY regular trading session closed before submission')
    if current['status'] != 'Normal':
        raise ValueError('SPY trading status changed before submission')
    shares, available_shares = position(trade)
    if shares != integer(request['basis_shares'], zero=True):
        raise ValueError('SPY holdings changed before submission')
    if any(enum(o.status) not in ('Filled', 'Canceled', 'Rejected', 'Expired', 'PartialWithdrawal')
           for o in trade.today_orders()):
        raise ValueError('active order appeared before submission')
    price = money(current['price'])
    estimated = request['quantity'] * price * Decimal('1.01')
    total, available = cash(trade)
    equity = money(total, zero=True) + shares * price
    if type(policy['max_order_bps']) is not int or not 1 <= policy['max_order_bps'] <= 10000:
        raise ValueError('typed allocation step limit required')
    if estimated > equity * policy['max_order_bps'] / 10000:
        raise ValueError('market price exceeds configured allocation step')
    if request['side'] == 'Buy':
        reserve = equity * policy['cash_buffer_bps'] / 10000
        if estimated > money(available, zero=True) - reserve:
            raise ValueError('settled cash changed before submission')
    elif request['quantity'] > available_shares:
        raise ValueError('sell shares changed before submission')
    now = datetime.now(timezone.utc)
    if now >= timestamp(request['not_after']):
        raise ValueError('allocation decision expired before submission')
    if not timestamp(current['regular_open_at']) <= now < timestamp(current['regular_close_at']):
        raise ValueError('SPY regular trading session closed before submission')
    if not 0 <= (now - timestamp(current['at'])).total_seconds() <= policy['quote_max_age_seconds']:
        raise ValueError('SPY quote expired before submission')
    return dict(symbol='SPY.US', order_type=OrderType.MO,
        side=OrderSide.Buy if request['side'] == 'Buy' else OrderSide.Sell,
        submitted_quantity=Decimal(request['quantity']), time_in_force=TimeInForceType.Day,
        outside_rth=OutsideRTH.RTHOnly, remark=request['remark'], client_request_id=request['client_request_id'])


def submit(asset, trade, quote, expected, request, policy):
    try:
        options = prepare_submission(asset, trade, quote, expected, request, policy)
    except Exception:
        raise OrderNotSubmitted('SDK preflight refused before sending an order') from None
    # Any exception from this point may mean the broker accepted the order.
    result = trade.submit_order(**options)
    return {'order_id': result.order_id}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--access-region', choices=('global', 'cn'), default='global')
    parser.add_argument('--client-id', required=True)
    parser.add_argument('--account', required=True)
    parser.add_argument('--cash-buffer-bps', type=int, default=200)
    parser.add_argument('--max-order-bps', type=int, default=1000)
    parser.add_argument('--quote-max-age-seconds', type=int, default=60)
    parser.add_argument('method', choices=('login', 'snapshot', 'submit'))
    parser.add_argument('--request', default='{}')
    args = parser.parse_args()
    identifier(args.client_id); identifier(args.account)
    try:
        asset, trade, quote = contexts(args.client_id, args.method == 'login', args.access_region)
    except Exception:
        if args.method == 'submit':
            raise OrderNotSubmitted('SDK preflight refused before sending an order') from None
        raise
    if args.method == 'login':
        identity(asset, trade, args.account)
        print('Official PAPER account verified; automatic SPY execution can be configured.')
        return
    request = json.loads(args.request)
    result = snapshot(asset, trade, quote, args.account, request) if args.method == 'snapshot' else submit(asset, trade, quote, args.account, request, {
        'cash_buffer_bps': args.cash_buffer_bps, 'max_order_bps': args.max_order_bps,
        'quote_max_age_seconds': args.quote_max_age_seconds})
    print(json.dumps(result, allow_nan=False))


def run():
    try:
        main()
    except OrderNotSubmitted:
        print(json.dumps({'status': 'not_submitted'}))
    except Exception:
        # No broker exception, signed statement URL, account details or token in tools/logs.
        print('Official SDK operation failed; check authorization and reconcile before retrying.', file=sys.stderr)
        raise SystemExit(1)


if __name__ == '__main__':
    run()
