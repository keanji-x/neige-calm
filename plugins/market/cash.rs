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

/// Work from the JSON number's decimal representation, not binary x*100.
/// Checked coefficient/exponent arithmetic accepts 0.29 and 1e2 without
/// admitting 0.001, saturation or an unrepresentable public amount.
fn decimal_cents(number: &serde_json::Number) -> Option<u64> {
    let value = number.as_f64()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let text = number.to_string();
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (significand, exponent) = text
        .split_once(['e', 'E'])
        .map_or(Some((text, 0_i32)), |(s, e)| {
            e.parse::<i32>().ok().map(|e| (s, e))
        })?;
    let (whole, fraction) = significand.split_once('.').unwrap_or((significand, ""));
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

pub(super) fn number(cents: u64) -> Option<f64> {
    if cents > MAX_CENTS {
        return None;
    }
    let value = cents as f64 / 100.0;
    let wire = serde_json::Number::from_f64(value)?;
    (decimal_cents(&wire) == Some(cents)).then_some(value)
}

pub(super) fn cents(value: &Value) -> Result<u64, String> {
    value.as_number().and_then(decimal_cents)
        .filter(|cents| number(*cents).is_some())
        .ok_or_else(|| "amount must be a finite nonnegative JSON number in whole cents within the safe numeric range; it is never rounded".into())
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
        if value.is_null() {
            return Ok(Self::default());
        }
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
    let result = rpc.call("neige.kv.get", json!({"key":format!("{PREFIX}{track}")}))?;
    Book::from_value(
        result
            .get("value")
            .ok_or("cash read returned no value field")?,
    )
}

pub(super) fn call(
    rpc: &Rpc,
    wake: &mpsc::Sender<()>,
    track: &str,
    name: &str,
    args: &Value,
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
        let amount = match object
            .get("amount")
            .ok_or("cash amount missing".to_string())
            .and_then(cents)
        {
            Ok(value) => value,
            Err(error) => return tool_error(error),
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
