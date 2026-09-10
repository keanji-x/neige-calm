//! Cash is a versioned balance document, never a quoted security.
use super::*;

pub(super) const PREFIX: &str = "cash/";
// Every public cent integer must also survive the JSON-number boundary.
const MAX_CENTS: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Balance {
    pub currency: Currency,
    pub cents: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Book {
    pub balances: Vec<Balance>,
}

fn currency(raw: &str) -> Result<Currency, String> {
    match raw {
        "CNY" => Ok(Currency::Cny),
        "USD" => Ok(Currency::Usd),
        "HKD" => Ok(Currency::Hkd),
        _ => Err("cash currency must be CNY, USD or HKD".into()),
    }
}

/// Whole cents from a decimal literal — `<digits>[.<digits>][e[±]<digits>]`
/// — read as digits, never as `value * 100.0`. Checked coefficient/exponent
/// arithmetic accepts `0.29` and `1e2` without admitting `0.001`,
/// saturation, a sign, or an unrepresentable public amount.
fn decimal_text_cents(text: &str) -> Option<u64> {
    let (significand, exponent) = text
        .split_once(['e', 'E'])
        .map_or(Some((text, 0_i32)), |(s, e)| {
            e.parse::<i32>().ok().map(|e| (s, e))
        })?;
    let (whole, fraction) = match significand.split_once('.') {
        // A decimal point commits the literal to digits on both sides:
        // `1.`, `.5` and `1.2.3` are not amounts.
        Some((_, "")) => return None,
        Some(parts) => parts,
        None => (significand, ""),
    };
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) {
        return None;
    }
    let coefficient = format!("{whole}{fraction}").parse::<u128>().ok()?;
    let power = exponent
        .checked_add(2)?
        .checked_sub(i32::try_from(fraction.len()).ok()?)?;
    let cents = if power >= 0 {
        coefficient.checked_mul(10_u128.checked_pow(power as u32)?)?
    } else {
        let divisor = 10_u128.checked_pow(power.unsigned_abs())?;
        if !coefficient.is_multiple_of(divisor) {
            return None;
        }
        coefficient / divisor
    };
    let cents = u64::try_from(cents).ok()?;
    (cents <= MAX_CENTS).then_some(cents)
}

/// The decimal `serde_json` still holds for a parsed number. Sound for the
/// numbers this plugin itself WROTE (`number` below round-trips them), which
/// is what [`Book::from_value`] reads back; it is NOT sound for a number a
/// caller sent, because parsing already replaced those digits with the
/// nearest `f64` — see [`wire_cents`].
fn decimal_cents(number: &serde_json::Number) -> Option<u64> {
    decimal_text_cents(&number.to_string())
}

pub(super) fn number(cents: u64) -> Option<f64> {
    if cents > MAX_CENTS {
        return None;
    }
    let value = cents as f64 / 100.0;
    let wire = serde_json::Number::from_f64(value)?;
    (decimal_cents(&wire) == Some(cents)).then_some(value)
}

pub(super) fn cents(value: &Value) -> Result<u64, String> {
    value
        .as_number()
        .and_then(decimal_cents)
        .filter(|cents| number(*cents).is_some())
        .ok_or_else(|| STORED_AMOUNT_ERROR.to_string())
}

const STORED_AMOUNT_ERROR: &str = "amount must be a finite nonnegative JSON number in whole cents within the safe numeric range; it is never rounded";
const WIRE_AMOUNT_ERROR: &str = "amount must be a nonnegative decimal in whole cents within the safe numeric range, and is never rounded; send it as a JSON string (\"0.29\") when the digits matter, because a raw JSON number is already an f64 by the time it is read";

/// Whole cents from the amount EXACTLY as the caller wrote it: the original
/// token text, either a JSON string holding a decimal literal or a bare JSON
/// number.
///
/// Validating the parsed `f64` instead would destroy the evidence it is
/// supposed to weigh: `35184372088832.001` parses to the whole-cent double
/// `35184372088832`, and `90071992547409.91` to `…409.90625`, whose own
/// shortest decimal is `…409.9`. Both would then "round-trip" as amounts
/// nobody sent. Read as text, the sub-cent digit and the unrepresentable
/// cent are both refused.
///
/// Only the string form is exact end to end: a raw JSON number has already
/// been through one `f64` in any kernel that re-serialises the frame.
pub(super) fn wire_cents(raw: &str) -> Result<u64, String> {
    let text = raw.trim();
    let unquoted = text
        .starts_with('"')
        .then(|| serde_json::from_str::<String>(text).ok())
        .flatten();
    let decimal = match text.starts_with('"') {
        true => unquoted.as_deref(),
        false => Some(text),
    };
    decimal
        .and_then(decimal_text_cents)
        .filter(|cents| number(*cents).is_some())
        .ok_or_else(|| WIRE_AMOUNT_ERROR.to_string())
}

pub(super) fn quantize(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    cents(&json!(round_to(value, 2))).ok()
}

pub(super) fn sum(mut values: impl Iterator<Item = u64>) -> Option<(u64, f64)> {
    let cents = values.try_fold(0_u64, |sum, value| sum.checked_add(value))?;
    number(cents).map(|value| (cents, value))
}

impl Book {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("cash document must be an object")?;
        if object.len() != 2 || object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("unsupported or malformed cash document version".into());
        }
        let rows = object
            .get("balances")
            .and_then(Value::as_array)
            .ok_or("cash balances must be an array")?;
        let mut balances = Vec::<Balance>::new();
        for row in rows {
            let row = row.as_object().ok_or("cash balance must be an object")?;
            if row.len() != 2 {
                return Err("cash balance requires only currency and amount".into());
            }
            let currency = currency(
                row.get("currency")
                    .and_then(Value::as_str)
                    .ok_or("cash currency missing")?,
            )?;
            if balances.iter().any(|balance| balance.currency == currency) {
                return Err("duplicate cash currency".into());
            }
            let cents = cents(row.get("amount").ok_or("cash amount missing")?)?;
            balances.push(Balance { currency, cents });
        }
        balances.sort_by_key(|balance| balance.currency.code());
        Ok(Self { balances })
    }

    pub fn to_value(&self) -> Value {
        json!({"version":1,"balances":self.balances.iter().map(|balance|json!({
            "currency":balance.currency.code(),"amount":number(balance.cents).expect("validated cash amount")
        })).collect::<Vec<_>>()})
    }
}

pub(super) fn load(rpc: &Rpc, track: &str) -> Result<Book, String> {
    match portfolio_state::read_key(rpc, &format!("{PREFIX}{track}"))? {
        None => Ok(Book::default()),
        Some(value) => Book::from_value(&value),
    }
}

/// `amount_text` is the amount member's original wire text, recovered from
/// the frame the kernel sent (`raw_argument`). It is deliberately separate
/// from `args`: whether the caller SENT an amount is answered by `args`
/// alone, so "no amount" and "an amount this plugin cannot read exactly"
/// stay two outcomes rather than one missing value.
pub(super) fn call(
    rpc: &Rpc,
    wake: &mpsc::Sender<()>,
    track: &str,
    name: &str,
    args: &Value,
    amount_text: Option<&str>,
) -> Value {
    let object = match args.as_object() {
        Some(object) => object,
        None => return tool_error("cash arguments must be an object"),
    };
    let update = if name == "market.cash.set" {
        if object.len() != 2 {
            return tool_error("cash.set requires only currency and amount");
        }
        let raw = match object.get("currency").and_then(Value::as_str) {
            Some(raw) => raw,
            None => return tool_error("cash currency missing"),
        };
        let currency = match currency(&raw.trim().to_ascii_uppercase()) {
            Ok(value) => value,
            Err(error) => return tool_error(error),
        };
        let amount = match (object.get("amount"), amount_text) {
            (None, _) => return tool_error("cash amount missing"),
            // Present on the frame, but its digits did not survive to here.
            // Refusing is the only honest answer: the parsed value is an
            // f64 that may already have rounded what the caller wrote.
            (Some(_), None) => {
                return tool_error(
                    "cash amount could not be read exactly as it was sent; resend it as a JSON string",
                );
            }
            (Some(_), Some(text)) => match wire_cents(text) {
                Ok(value) => value,
                Err(error) => return tool_error(error),
            },
        };
        Some(Balance {
            currency,
            cents: amount,
        })
    } else {
        if !object.is_empty() {
            return tool_error("cash.list accepts no arguments");
        }
        None
    };
    let _state = match portfolio_state::STATE.lock() {
        Ok(lock) => lock,
        Err(_) => return tool_error("portfolio state lock unavailable"),
    };
    let mut book = match load(rpc, track) {
        Ok(book) => book,
        Err(error) => return tool_error(format!("Could not read cash — {error}")),
    };
    let writing = update.is_some();
    if let Some(balance) = update {
        book.balances.retain(|old| old.currency != balance.currency);
        book.balances.push(balance);
        book.balances.sort_by_key(|balance| balance.currency.code());
        if let Err(error) = rpc.call(
            "neige.kv.set",
            json!({"key":format!("{PREFIX}{track}"),"value":book.to_value()}),
        ) {
            return tool_error(format!(
                "Could not confirm cash saved — {error}. Read cash.list before retrying."
            ));
        }
    }
    drop(_state);
    if writing {
        let _ = wake.send(());
    }
    text_result(
        if writing {
            "Cash balance saved; portfolio refresh queued."
        } else {
            "Saved current cash balances."
        }
        .into(),
        json!({"balances":book.to_value()["balances"],"refresh":if writing{Some("queued")}else{None}}),
    )
}

#[cfg(test)]
#[path = "cash_tests.rs"]
mod tests;
