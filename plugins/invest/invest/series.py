"""`series_show`: the `market.series` contract for US symbols, over the official SDK's daily
candlesticks (#2104 §3.6). Only the chart resolver calls it.

Per request: every key is required and checked, and a request past its deadline is refused before
any SDK call. One SDK subprocess then answers every US symbol: first the newest daily bar the
source lists (`complete_through`), then the window `[start - 14 days, as_of]`. Per symbol: the
depth and near-end checks, weekly (ISO) or monthly aggregation of the daily bars, and the inclusion
rule. US is never relaxed, so a period is included only when its end is earlier than
`complete_through`: a later daily bar proves it closed, in `live` and `frozen` mode alike.
"""
import calendar
from datetime import date, timedelta
from decimal import Decimal, InvalidOperation
import math
import re

from .broker import BrokerError
from .errors import FORBIDDEN, Refused, served
from .symbols import canonical, to_sdk

TOOL = 'series_show'
TRACK_KEY, CALLER_KEY = 'dev.neige/track', 'dev.neige/caller'
FIELDS = ('open', 'high', 'low', 'close', 'volume')
PERIODS = ('day', 'week', 'month')
MODES = ('live', 'frozen')
KEYS = ('series', 'fields', 'period', 'mode', 'start', 'as_of', 'deadline_ms')
MAX_SERIES = 8
MARGIN_DAYS = 14
MS_PER_DAY = 86_400_000
CURRENCY = 'USD'
DATE = re.compile(r'[0-9]{4}-[0-9]{2}-[0-9]{2}')
EPOCH = date(1970, 1, 1)


def admit(meta):
    """The chart resolver's call shape (§3.6, F4): the host's Track context and no agent caller."""
    meta = meta if isinstance(meta, dict) else {}
    track = meta.get(TRACK_KEY)
    if not isinstance(track, dict) or not isinstance(track.get('id'), str) or not track['id']:
        raise Refused(FORBIDDEN, f'{served(TOOL)}: serves only the chart resolver: host Track context required')
    if CALLER_KEY in meta:
        raise Refused(FORBIDDEN, f'{served(TOOL)}: serves only the chart resolver, never an agent')


def calendar_date(raw):
    if not isinstance(raw, str) or not DATE.fullmatch(raw):
        return None
    try:
        return date.fromisoformat(raw)
    except ValueError:
        return None


def parse_request(args):
    """Every key is required and checked; nothing is defaulted. Each error names its key."""
    if not isinstance(args, dict):
        raise ValueError('arguments must be an object')
    for key in KEYS:
        if key not in args:
            raise ValueError(f'`{key}` is required')
    unknown = sorted(set(args) - set(KEYS))
    if unknown:
        raise ValueError(f'unknown argument `{unknown[0]}`')
    series = args['series']
    if not isinstance(series, list) or not 1 <= len(series) <= MAX_SERIES \
            or not all(isinstance(item, str) for item in series):
        raise ValueError(f'`series` must be an array of 1 to {MAX_SERIES} asset name strings')
    fields = args['fields']
    if not isinstance(fields, list) or not 1 <= len(fields) <= 5 or not all(f in FIELDS for f in fields):
        raise ValueError('`fields` must list 1 to 5 of open, high, low, close, volume')
    if args['period'] not in PERIODS:
        raise ValueError(f"`period` must be day, week or month, got `{args['period']}`")
    # No default on purpose: `live` must never be assumed for a caller that did not say it.
    if args['mode'] not in MODES:
        raise ValueError(f"`mode` must be live or frozen, got `{args['mode']}`")
    start, as_of = calendar_date(args['start']), calendar_date(args['as_of'])
    if start is None:
        raise ValueError(f"`start` must be a calendar date YYYY-MM-DD, got `{args['start']}`")
    if as_of is None:
        raise ValueError(f"`as_of` must be a calendar date YYYY-MM-DD, got `{args['as_of']}`")
    if start > as_of:
        raise ValueError('`start` must not be later than `as_of`')
    if type(args['deadline_ms']) is not int:
        raise ValueError('`deadline_ms` must be an integer (unix milliseconds)')
    return {'series': series, 'fields': fields, 'period': args['period'], 'start': start, 'as_of': as_of,
            'deadline_ms': args['deadline_ms']}


def period_bounds(period, day):
    if period == 'week':
        first = day - timedelta(days=day.weekday())
        return first, first + timedelta(days=6)
    if period == 'month':
        return day.replace(day=1), day.replace(day=calendar.monthrange(day.year, day.month)[1])
    return day, day


def number(raw):
    try:
        value = float(Decimal(raw)) if type(raw) in (str, int, Decimal) else math.nan
    except InvalidOperation:
        value = math.nan
    if not math.isfinite(value):
        raise ValueError('the source answered a non-numeric bar value')
    return value


def daily(answer, lo, hi):
    """The bridge's `[date, open, high, low, close, volume]` rows inside `[lo, hi]`, ascending by date."""
    bars = {}
    for row in answer.get('bars', ()):
        if not isinstance(row, list) or len(row) != 6 or calendar_date(row[0]) is None:
            raise ValueError('the source answered a malformed bar')
        day = calendar_date(row[0])
        if day in bars:
            raise ValueError('the source answered one day twice')
        if lo <= day <= hi:
            bars[day] = dict(zip(FIELDS, map(number, row[1:])))
    return [(day, bars[day]) for day in sorted(bars)]


def aggregate(bars, period):
    """Open of the first day, close of the last, max high, min low, summed volume."""
    candles = []
    for day, bar in bars:
        first, last = period_bounds(period, day)
        if candles and candles[-1][0] == first:
            candle = candles[-1][2]
            candle['high'], candle['low'] = max(candle['high'], bar['high']), min(candle['low'], bar['low'])
            candle['close'] = bar['close']
            candle['volume'] += bar['volume']
        else:
            candles.append((first, last, dict(bar)))
    return candles


def resolve(request, answer):
    """One symbol's points under the depth, near-end and inclusion rules; ValueError is `unavailable`."""
    if not isinstance(answer, dict):
        raise ValueError('the source answered nothing for this asset')
    if 'error' in answer:
        raise ValueError(str(answer['error']))
    complete_through = calendar_date(answer.get('complete_through'))
    if complete_through is None:
        raise ValueError('probe returned no recent bar')
    start, as_of = request['start'], request['as_of']
    bars = daily(answer, start - timedelta(days=MARGIN_DAYS), as_of)
    if bars and bars[0][0] > start + timedelta(days=MARGIN_DAYS):
        raise ValueError('lookback exceeds source depth')
    in_window = [(day, bar) for day, bar in bars if day >= start]
    if not in_window or in_window[-1][0] < as_of - timedelta(days=MARGIN_DAYS):
        raise ValueError('no data near cutoff')
    points = [[(first - EPOCH).days * MS_PER_DAY, *(candle[f] for f in request['fields'])]
              for first, last, candle in aggregate(in_window, request['period'])
              if first >= start and last <= as_of and last < complete_through]
    if len(points) < 2:
        raise ValueError('no data in range')
    return complete_through.isoformat(), points


def tool_error(text):
    return {'isError': True, 'content': [{'type': 'text', 'text': f'{served(TOOL)}: {text}'}]}


def show(broker, args, now_ms):
    """The tool reply: `{series: [{asset, currency?, status, complete_through?, points?, reason?}]}`."""
    try:
        request = parse_request(args)
    except ValueError as error:
        return tool_error(str(error))
    # The resolver gave up on this call already; answering would spend an SDK call nobody reads.
    if request['deadline_ms'] < now_ms:
        return tool_error('deadline exceeded')
    assets = {}
    for raw in request['series']:
        try:
            assets[raw] = canonical(raw)
        except ValueError as error:
            assets[raw] = error
    symbols = sorted({to_sdk(s) for s in assets.values() if isinstance(s, str)})
    answers, failure = {}, None
    if symbols:
        try:
            reply = broker.series(symbols, (request['start'] - timedelta(days=MARGIN_DAYS)).isoformat(),
                                  request['as_of'].isoformat(), (request['deadline_ms'] - now_ms) / 1000)
            answers = reply.get('series') if isinstance(reply.get('series'), dict) else {}
        except BrokerError as error:
            failure = str(error)
    entries, summary = [], []
    for raw in request['series']:
        symbol = assets[raw]
        if not isinstance(symbol, str):
            entries.append({'asset': raw, 'status': 'unknown_asset', 'reason': str(symbol)})
            summary.append(f'{raw}: unknown_asset ({symbol})')
            continue
        try:
            if failure:
                raise ValueError(failure)
            complete_through, points = resolve(request, answers.get(to_sdk(symbol)))
        except ValueError as error:
            entries.append({'asset': raw, 'status': 'unavailable', 'reason': str(error)})
            summary.append(f'{raw}: unavailable ({error})')
            continue
        entries.append({'asset': raw, 'currency': CURRENCY, 'status': 'ok', 'complete_through': complete_through,
                        'points': points})
        summary.append(f'{raw}: ok, {len(points)} points through {complete_through} ({CURRENCY})')
    return {'content': [{'type': 'text', 'text': '; '.join(summary)}], 'structuredContent': {'series': entries}}
