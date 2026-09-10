//! Cash-inclusive views share public cent values; legacy security totals do not change.
use super::*;

pub(super) struct Combined {
    pub allocation: Vec<Value>,
    pub positions: Vec<Value>,
    pub cash: Vec<Value>,
    pub total: Option<(f64, String)>,
    pub recorded: bool,
    pub caption: String,
}

pub(super) fn value(
    cfg: &Config,
    securities: &PricedPortfolio,
    book: &Result<cash::Book, String>,
    holdings_valid: bool,
    cache: &mut PassCache,
) -> Combined {
    let mut positions = securities.rows.clone();
    let mut allocation = Vec::new();
    let mut amounts = Vec::<u64>::new();
    let mut currencies = Vec::<String>::new();
    let mut complete = securities.complete && holdings_valid;
    let mut reasons = Vec::<String>::new();
    if !holdings_valid {
        reasons.push("Some stored securities are unreadable".into());
    }
    let mut conversions = securities.conversions.clone();
    for row in &mut positions {
        let cents = row.get("value").and_then(|v| cash::cents(v).ok());
        let unit = securities
            .settlement
            .map(|unit| unit.code().to_string())
            .or_else(|| row["currency"].as_str().map(str::to_string));
        if let (Some(cents), Some(unit)) = (cents, unit.as_deref()) {
            amounts.push(cents);
            if !currencies.iter().any(|old| old == unit) {
                currencies.push(unit.into());
            }
        } else {
            complete = false;
            row["value"] = Value::Null;
        }
        row["value_currency"] = json!(unit);
        let venue = row["venue"].as_str().unwrap_or("");
        let asset = row["asset"].as_str().unwrap_or("");
        allocation.push(
            json!({"id":format!("security:{venue}:{asset}"),"label":format!("{venue}:{asset}"),
            "kind":"security","value":row["value"],"currency":unit}),
        );
    }
    let mut cash_rows = Vec::new();
    let cash_recorded = match book {
        Ok(book) => {
            for balance in &book.balances {
                let amount = cash::number(balance.cents).expect("validated cash cents");
                let to = securities.settlement.unwrap_or(balance.currency);
                let conversion = if balance.cents == 0 {
                    Ok((0, None))
                } else {
                    fx_path(cfg, balance.currency, to, &mut cache.rates).and_then(|path| {
                        let cents = cash::quantize(amount * path.factor).ok_or_else(|| {
                            format!(
                                "{} cash value exceeds the safe cent range",
                                balance.currency.code()
                            )
                        })?;
                        let rate = if path.hops.is_empty() {
                            None
                        } else {
                            let description = path.describe();
                            if !conversions.contains(&description) {
                                conversions.push(description);
                            }
                            Some(round_to(path.factor, 8))
                        };
                        Ok((cents, rate))
                    })
                };
                let (valued, rate) = match conversion {
                    Ok((cents, rate)) => {
                        amounts.push(cents);
                        if !currencies.iter().any(|old| old == to.code()) {
                            currencies.push(to.code().into());
                        }
                        (cash::number(cents), rate)
                    }
                    Err(error) => {
                        complete = false;
                        reasons.push(format!("{} cash: {error}", balance.currency.code()));
                        (None, None)
                    }
                };
                cash_rows.push(
                    json!({"currency":balance.currency.code(),"amount":amount,"value":valued,
                    "value_currency":to.code(),"rate":rate}),
                );
                allocation.push(json!({"id":format!("cash:{}",balance.currency.code()),"label":format!("Cash {}",balance.currency.code()),
                    "kind":"cash","value":valued,"currency":to.code()}));
            }
            !book.balances.is_empty()
        }
        Err(error) => {
            complete = false;
            reasons.push(format!("Cash balances unavailable — {error}"));
            cash_rows.push(json!({"currency":Value::Null,"amount":Value::Null,"value":Value::Null,"value_currency":Value::Null,"rate":Value::Null}));
            allocation.push(json!({"id":"cash-unavailable","label":"Cash unavailable","kind":"cash","value":Value::Null,"currency":Value::Null}));
            true
        }
    };
    let recorded = !positions.is_empty() || cash_recorded || !holdings_valid;
    if !securities.complete {
        reasons.push("Some securities could not be valued".into());
    }
    if currencies.len() > 1 {
        complete = false;
        reasons.push("Recorded assets have incompatible currencies".into());
    }
    let unit = currencies
        .first()
        .cloned()
        .or_else(|| securities.settlement.map(|c| c.code().into()));
    let cent_total = if complete {
        cash::sum(amounts.into_iter())
    } else {
        None
    };
    if complete && cent_total.is_none() {
        reasons.push("Combined value exceeds the safe cent range".into());
    }
    let total = cent_total.and_then(|(_, amount)| unit.clone().map(|unit| (amount, unit)));
    for row in positions.iter_mut().chain(cash_rows.iter_mut()) {
        row["weight"] = match (
            cent_total,
            row.get("value").and_then(|v| cash::cents(v).ok()),
            &total,
        ) {
            (Some((sum, _)), Some(cents), Some(_)) if sum > 0 => {
                json!(100.0 * cents as f64 / sum as f64)
            }
            _ => Value::Null,
        };
    }
    allocation.push(json!({"id":"total","label":"Total","kind":"total",
        "value":total.as_ref().map(|(value,_)|*value),"currency":total.as_ref().map(|(_,unit)|unit.as_str())}));
    let mut caption = if !recorded {
        "No recorded assets".into()
    } else if let Some((_, unit)) = &total {
        format!(
            "Recorded securities and cash, totalled in {unit}; value changes include balance changes, not investment returns"
        )
    } else {
        "Combined total and weights unavailable; no subset is presented as the portfolio".into()
    };
    for detail in reasons.iter().chain(conversions.iter()) {
        caption.push_str(". ");
        caption.push_str(detail);
    }
    Combined {
        allocation,
        positions,
        cash: cash_rows,
        total,
        recorded,
        caption,
    }
}

pub(super) fn table(columns: &[(&str, &str)], rows: Vec<Value>, at: &str, caption: &str) -> Value {
    json!({"columns":columns.iter().map(|(key,label)|json!({"key":key,"label":label})).collect::<Vec<_>>(),
        "rows":rows,"as_of":at,"caption":format!("Observed at {at} — {caption}")})
}

impl Combined {
    pub fn projections(&self, at: &str) -> Vec<(&'static str, Value)> {
        vec![
            (
                "portfolio.allocation",
                table(
                    &[
                        ("label", "Asset"),
                        ("value", "Value"),
                        ("currency", "Currency"),
                    ],
                    self.allocation.clone(),
                    at,
                    &self.caption,
                ),
            ),
            (
                "portfolio.positions",
                table(
                    &[
                        ("asset", "Asset"),
                        ("venue", "Venue"),
                        ("qty", "Quantity"),
                        ("price", "Price"),
                        ("currency", "Priced in"),
                        ("value", "Value"),
                        ("value_currency", "Value currency"),
                        ("weight", "Weight (%)"),
                    ],
                    self.positions.clone(),
                    at,
                    &self.caption,
                ),
            ),
            (
                "portfolio.cash",
                table(
                    &[
                        ("currency", "Currency"),
                        ("amount", "Current balance"),
                        ("value", "Converted value"),
                        ("value_currency", "Value currency"),
                        ("weight", "Weight (%)"),
                    ],
                    self.cash.clone(),
                    at,
                    &self.caption,
                ),
            ),
        ]
    }
}
