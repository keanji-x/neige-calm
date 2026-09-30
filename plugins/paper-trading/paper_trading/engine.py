"""Track-scoped decisions and the supervised broker state machine."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from fractions import Fraction
import json
import re
from zoneinfo import ZoneInfo

from .config import broker_money, calendar_date, exact, identifier, integer, money, symbol, timestamp
from .ledger import Ledger, encoded
from .portfolio import BROKER_TERMINAL, TERMINAL, cost_exposure, trades
from . import research
from .reconcile import refresh, remark, verify_order


def utc_now():
    return datetime.now(timezone.utc)


class Engine:
    def __init__(self, root, config, broker, clock=None, *, strategy_revision=None):
        self.config = config
        self.broker = broker
        self.clock = clock or utc_now
        # None identifies legacy/domain callers; never attribute their historical
        # decisions to a newly approved strategy on a retry or import.
        if strategy_revision is not None and (not isinstance(strategy_revision, str)
                                              or re.fullmatch('[0-9a-f]{64}', strategy_revision) is None):
            raise ValueError('invalid approved strategy revision')
        self.strategy_revision = strategy_revision
        self.ledger = Ledger(root, config)

    def authorize(self, track):
        if track != self.config.owner_track_id:
            raise ValueError("this Track does not own the configured paper portfolio")

    def call(self, track, name, args):
        self.authorize(track)
        with self.ledger.session() as db:
            if name == "paper.ingest":
                return research.ingest(db, self.ledger, self.config, args)
            if name == "paper.decide":
                return self.decide(db, args)
            if name == "paper.review":
                return self.review(db, args)
            if name == "paper.pause":
                exact(args, {"paused"})
                if type(args["paused"]) is not bool:
                    raise ValueError("paused must be boolean")
                self.ledger.set_meta(db, "paused", args["paused"])
                self.ledger.event(db, "pause_changed", args)
            elif name == "paper.refresh":
                exact(args, set())
                self.ledger.set_meta(db, "refresh_requested", True)
            elif name in ("paper.status", "paper.journal"):
                exact(args, set())
            else:
                raise ValueError("unknown tool")
            return self.status(db)

    def decide(self, db, args):
        base = {"decision_id", "cycle_id", "source_id", "prediction_id", "trade_id", "action",
                "symbol", "rationale", "valid_until"}
        if not isinstance(args, dict):
            raise ValueError("decision must be an object")
        action = args.get("action")
        required = base | ({"quantity", "limit_price", "stop_price", "target_price"} if action == "buy"
                           else {"quantity", "limit_price"} if action == "sell" else set())
        exact(args, required)
        for key in ("decision_id", "cycle_id", "trade_id", "prediction_id"):
            identifier(args[key])
        old = db.execute("SELECT body FROM decisions WHERE id=?", (args["decision_id"],)).fetchone()
        if old is not None:
            if old[0] != encoded(args):
                raise ValueError("decision ID already names a different immutable request")
            return self.ledger.decision(db, args["decision_id"])
        if action not in ("buy", "sell", "hold"):
            raise ValueError("action must be buy, sell or hold; shorting is unsupported")
        symbol(args["symbol"])
        if action == "buy" and args["symbol"] not in self.config.symbols:
            raise ValueError("symbol is not allowlisted")
        if not isinstance(args["rationale"], str) or not 10 <= len(args["rationale"]) <= 6000:
            raise ValueError("rationale must contain 10-6000 characters")
        now = self.clock()
        deadline = timestamp(args["valid_until"])
        if not now < deadline <= now + timedelta(days=1):
            raise ValueError("decision validity must end within the next 24 hours")
        src = research.source(db, args["source_id"])
        predictions = [p for p in src["predictions"] if p["id"] == args["prediction_id"]]
        if len(predictions) != 1 or predictions[0]["symbol"] + ".US" != args["symbol"]:
            raise ValueError("prediction does not identify the requested symbol")
        prediction = predictions[0]
        if src["week"] > now.date().isoformat():
            raise ValueError("future research cannot be used")
        if action != "hold":
            if type(args["quantity"]) is not int:
                raise ValueError("quantity must be a JSON integer")
            integer(args["quantity"])
            limit = money(args["limit_price"])
            if limit < 1 or limit != limit.quantize(Decimal("0.01")):
                raise ValueError("first version requires prices >= USD 1 in cent increments")
        decisions = self.ledger.decisions(db)
        if action == "buy":
            if self.ledger.get_meta(db, "paused"):
                raise ValueError("new entries are paused")
            if prediction["direction"] != "long" or prediction["conviction_tier"] == "watch":
                raise ValueError("only explicit long, non-watch predictions can open positions")
            if deadline.astimezone(ZoneInfo("America/New_York")).date() > calendar_date(prediction["horizon_end"]):
                raise ValueError("decision outlives the research horizon")
            if not money(args["stop_price"]) < limit < money(args["target_price"]):
                raise ValueError("long entry requires stop < limit < target")
            if any(d["body"]["trade_id"] == args["trade_id"] for d in decisions):
                raise ValueError("trade ID already exists")
            if any(d["body"]["action"] == "buy" and d["body"]["source_id"] == args["source_id"]
                   and d["body"]["prediction_id"] == args["prediction_id"]
                   and d["state"] not in ("rejected", "expired") for d in decisions):
                raise ValueError("prediction already has an entry decision")
        elif action == "sell":
            owned = [t for t in trades(decisions, self.ledger.fills(db)) if t["trade_id"] == args["trade_id"]]
            if len(owned) != 1 or owned[0]["symbol"] != args["symbol"] or owned[0]["quantity"] < args["quantity"]:
                raise ValueError("exit must identify an owned trade and quantity")
        db.execute("INSERT INTO decisions(id,body,state,created_at) VALUES (?,?,?,?)",
                   (args["decision_id"], encoded(args), "recorded" if action == "hold" else "queued", now.isoformat()))
        self.ledger.event(db, "decision_recorded", args)
        if self.strategy_revision is not None:
            self.ledger.set_meta(db, 'decision_strategy:' + args['decision_id'], self.strategy_revision)
            self.ledger.event(db, 'decision_strategy_bound', {
                'decision_id': args['decision_id'], 'strategy_revision': self.strategy_revision})
        return self.ledger.decision(db, args["decision_id"])

    def quote(self, name):
        quote = self.broker.quote(name)
        bars = self.broker.intraday(name)
        if quote["status"] != "Normal" or not bars:
            raise ValueError("normal, timestamped regular-session market data required")
        latest = max(bars, key=lambda b: timestamp(b["time"]))
        age = (self.clock() - timestamp(latest["time"])).total_seconds()
        if not 0 <= age <= self.config.quote_max_age_seconds:
            raise ValueError("intraday market data is stale or future-dated")
        local = self.clock().astimezone(ZoneInfo("America/New_York"))
        if local.weekday() > 4 or not (9 * 60 + 30 <= local.hour * 60 + local.minute < 16 * 60):
            raise ValueError("outside US regular trading hours")
        last = broker_money(quote["last"])
        if abs(last / broker_money(latest["price"]) - 1) * 10000 > self.config.max_price_deviation_bps:
            raise ValueError("quote and timestamped intraday price disagree")
        return {"symbol": name, "last": str(last), "intraday_at": latest["time"],
                "observed_at": self.clock().isoformat()}

    def preflight(self, db, decision):
        if decision["state"] not in ("queued", "ready"):
            raise ValueError("decision is not eligible for submission; reconcile instead of retrying")
        plan = decision["body"]
        if timestamp(plan["valid_until"]) <= self.clock():
            raise ValueError("decision has expired")
        snapshot = self.ledger.get_meta(db, "snapshot")
        if snapshot is None or snapshot["unresolved"]:
            raise ValueError("account reconciliation is missing or has unknown submissions")
        decisions = self.ledger.decisions(db)
        fills = self.ledger.fills(db)
        portfolio = trades(decisions, fills)
        others = [d for d in decisions if d["id"] != decision["id"] and d["state"] not in TERMINAL]

        def remaining(d):
            return d["body"]["quantity"] - sum(f["quantity"] for f in fills if f["order_id"] == d["broker_id"])

        quote = self.quote(plan["symbol"])
        limit, qty = money(plan["limit_price"]), plan["quantity"]
        if abs(limit / broker_money(quote["last"]) - 1) * 10000 > self.config.max_price_deviation_bps:
            raise ValueError("limit price is too far from current market")
        if limit * qty > money(self.config.max_order_usd):
            raise ValueError("order notional exceeds configured limit")
        if plan["action"] == "buy":
            if plan["symbol"] not in self.config.symbols:
                raise ValueError("symbol is no longer allowlisted")
            if self.ledger.get_meta(db, "paused"):
                raise ValueError("new entries are paused")
            if not money(plan["stop_price"]) < broker_money(quote["last"]) < money(plan["target_price"]):
                raise ValueError("market has already crossed the entry thesis levels")
            if (limit - money(plan["stop_price"])) * qty > money(self.config.max_trade_risk_usd):
                raise ValueError("initial price risk exceeds configured limit")
            if any(t["symbol"] == plan["symbol"] and t["trade_id"] != plan["trade_id"]
                   and (t["quantity"] or t["state"] == "pending") for t in portfolio):
                raise ValueError("another trade already owns this symbol")
            buys = [d for d in others if d["body"]["action"] == "buy"]
            reserved = sum((money(d["body"]["limit_price"]) * remaining(d) for d in buys), Decimal(0))
            local_reserved = sum((money(d["body"]["limit_price"]) * remaining(d)
                                  for d in buys if not d["broker_id"]), Decimal(0))
            # Available broker cash/shares already exclude its active orders.
            if limit * qty + local_reserved > broker_money(snapshot["available_cash_usd"], zero=True):
                raise ValueError("cash is insufficient after outstanding reservations")
            gross = cost_exposure(portfolio, decisions, fills)
            if gross + Fraction(reserved) + Fraction(limit) * qty > Fraction(money(self.config.max_portfolio_usd)):
                raise ValueError("portfolio cost exposure exceeds configured limit")
        else:
            current = next((t for t in portfolio if t["trade_id"] == plan["trade_id"]), None)
            if current is None or current["symbol"] != plan["symbol"]:
                raise ValueError("owned trade missing")
            entry = self.ledger.decision(db, current["entry_decision_id"])
            if entry["state"] not in TERMINAL:
                raise ValueError("entry must be settled or canceled before exiting")
            sells = [d for d in others if d["body"]["action"] == "sell" and d["body"]["trade_id"] == plan["trade_id"]]
            reserved = sum(remaining(d) for d in sells)
            local_reserved = sum(remaining(d) for d in sells if not d["broker_id"])
            position = next((p for p in snapshot["positions"] if p["symbol"] == plan["symbol"]), None)
            if (position is None or qty + reserved > current["quantity"]
                    or qty + local_reserved > integer(position["available"], zero=True)):
                raise ValueError("exit exceeds available unreserved shares")
        return quote

    def process_once(self):
        try:
            with self.ledger.session() as db:
                refresh(db, self.ledger, self.config, self.broker, self.clock())
                self.ledger.set_meta(db, "refresh_requested", False)
                for decision in self.ledger.decisions(db):
                    if decision["state"] not in ("queued", "ready"):
                        continue
                    if timestamp(decision["body"]["valid_until"]) <= self.clock():
                        self.ledger.change(db, decision["id"], "expired", "decision expired before submission")
                        continue
                    try:
                        evidence = self.preflight(db, decision)
                        if decision["state"] != "ready":
                            self.ledger.event(db, "preflight", {"decision_id": decision["id"], "quote": evidence})
                        self.ledger.change(db, decision["id"], "ready")
                    except (ValueError, KeyError) as error:
                        self.ledger.change(db, decision["id"], "queued", str(error))
                alerts = []
                for trade in trades(self.ledger.decisions(db), self.ledger.fills(db)):
                    if not trade["quantity"]:
                        continue
                    try:
                        quote = self.quote(trade["symbol"])
                        if broker_money(quote["last"]) <= money(trade["stop_price"]):
                            alerts.append({"trade_id": trade["trade_id"], "reason": "stop_crossed", "quote": quote})
                        elif broker_money(quote["last"]) >= money(trade["target_price"]):
                            alerts.append({"trade_id": trade["trade_id"], "reason": "target_crossed", "quote": quote})
                    except Exception:
                        alerts.append({"trade_id": trade["trade_id"], "reason": "market_data_unavailable"})
                self.ledger.set_meta(db, "alerts", alerts)
        except Exception as error:
            # Roll back the entire inconsistent snapshot; preserve prior evidence.
            message = str(error) if isinstance(error, ValueError) else f"reconciliation failed ({type(error).__name__})"
            with self.ledger.session() as db:
                if self.ledger.get_meta(db, "error") != message:
                    self.ledger.event(db, "reconciliation_error", {"error": message})
                self.ledger.set_meta(db, "error", message)
        with self.ledger.session() as db:
            return self.status(db)

    def operator_args(self, db, key, cancel=False):
        self.broker.identity(self.config.account_no)
        decision = self.ledger.decision(db, identifier(key))
        if cancel:
            if not decision["broker_id"]:
                raise ValueError("cannot cancel without a known owned broker order")
            detail = self.broker.order_detail(decision["broker_id"])
            verify_order(detail, decision["body"])
            if detail["status"] in BROKER_TERMINAL:
                raise ValueError("broker order is already terminal")
            return self.broker.cancel_args(decision["broker_id"])
        refresh(db, self.ledger, self.config, self.broker, self.clock())
        evidence = self.preflight(db, self.ledger.decision(db, key))
        self.ledger.event(db, "operator_preflight", {"decision_id": key, "quote": evidence})
        plan = decision["body"]
        return self.broker.order_args(plan["symbol"], plan["action"], plan["quantity"], plan["limit_price"], remark(plan))

    def operator_preview(self, key, cancel=False):
        with self.ledger.session() as db:
            args = self.operator_args(db, key, cancel)
            return args, self.broker.preview(args)

    def operator_confirm(self, key, args, code, cancel=False):
        if not isinstance(code, str) or re.fullmatch(r"[0-9]{3}", code) is None:
            raise ValueError("invalid confirmation code")
        with self.ledger.session() as db:
            current_args = self.operator_args(db, key, cancel)
            if current_args != args:
                raise ValueError("order changed since preview")
            # Commit before the broker call, retaining the cross-process lock.
            if not cancel:
                self.ledger.change(db, key, "submitting")
            self.ledger.event(db, "cancel_requested" if cancel else "submission_started", {"decision_id": key})
            db.commit()
            try:
                result = self.broker.execute(args, code)
                if not cancel:
                    order_id = result["order_id"]
                    if not isinstance(order_id, str) or not order_id.strip():
                        raise ValueError("broker returned no order ID")
                    self.ledger.change(db, key, "working", broker_id=order_id)
                self.ledger.event(db, "cancel_acknowledged" if cancel else "submission_acknowledged", {"decision_id": key})
                db.commit()
            except Exception:
                if not cancel:
                    self.ledger.change(db, key, "unknown", "submission outcome unknown; do not retry")
                self.ledger.event(db, "cancel_unknown" if cancel else "submission_unknown", {"decision_id": key})
                db.commit()
                raise ValueError("broker outcome unknown; reconcile instead of retrying") from None
        return self.process_once()

    def review(self, db, args):
        exact(args, {"review_id", "trade_id", "evidence_revision", "analysis", "next_action"})
        identifier(args["review_id"])
        identifier(args["trade_id"])
        for key in ("analysis", "next_action"):
            if not isinstance(args[key], str) or not 10 <= len(args[key]) <= 6000:
                raise ValueError("review text must contain 10-6000 characters")
        old = db.execute("SELECT body FROM reviews WHERE id=?", (args["review_id"],)).fetchone()
        if old:
            if old[0] != encoded(args):
                raise ValueError("review ID already names a different immutable review")
            return args
        current = next((t for t in trades(self.ledger.decisions(db), self.ledger.fills(db))
                        if t["trade_id"] == args["trade_id"]), None)
        if current is None or current["state"] != "closed" or current["evidence_revision"] != args["evidence_revision"]:
            raise ValueError("review requires a closed trade and its current evidence revision")
        if self.ledger.get_meta(db, "error"):
            raise ValueError("resolve reconciliation errors before reviewing")
        db.execute("INSERT INTO reviews VALUES (?,?)", (args["review_id"], encoded(args)))
        self.ledger.event(db, "review_added", args)
        return args

    def status(self, db):
        decisions = self.ledger.decisions(db)
        fills = self.ledger.fills(db)
        decision_views = []
        for decision in decisions:
            revision = self.ledger.get_meta(db, 'decision_strategy:' + decision['id'])
            decision_views.append(decision | {'strategy_revision': revision} if revision is not None else decision)
        return {"mode": "supervised_paper", "paused": self.ledger.get_meta(db, "paused"),
                "snapshot": self.ledger.get_meta(db, "snapshot"), "error": self.ledger.get_meta(db, "error"),
                "alerts": self.ledger.get_meta(db, "alerts") or [], "decisions": decision_views,
                "trades": trades(decisions, fills), "fills": fills,
                "reviews": self.ledger.reviews(db),
                "journal": [dict(r) | {"body": json.loads(r["body"])} for r in
                            db.execute("SELECT * FROM journal ORDER BY seq DESC LIMIT 200")]}
