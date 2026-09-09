use super::*;

#[test]
fn decimal_cash_inputs_accept_cents_and_exponents_without_binary_integrality() {
    for (raw, expected) in [
        ("0", 0),
        ("0.01", 1),
        ("0.29", 29),
        ("1.20", 120),
        ("1e2", 10000),
        ("2.9e-1", 29),
    ] {
        let value: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(cents(&value), Ok(expected), "{raw}");
        assert_eq!(cents(&json!(number(expected).unwrap())), Ok(expected));
    }
}

#[test]
fn subcent_negative_non_numeric_and_unsafe_cash_are_refused_without_rounding() {
    for value in [
        json!(0.001),
        json!(1.005),
        json!(-0.01),
        json!(1e100),
        json!("0.29"),
        Value::Null,
        json!(true),
    ] {
        assert!(cents(&value).is_err(), "{value}");
    }
    assert!(number(MAX_CENTS + 1).is_none());
    assert!(quantize(f64::INFINITY).is_none());
    assert!(quantize(f64::NAN).is_none());
    assert!(sum([MAX_CENTS, 1].into_iter()).is_none());
}

#[test]
fn cash_document_is_versioned_strict_unique_and_keeps_explicit_zero() {
    assert!(Book::from_value(&Value::Null).unwrap().balances.is_empty());
    let zero = json!({"version":1,"balances":[{"currency":"CNY","amount":0.0}]});
    let book = Book::from_value(&zero).unwrap();
    assert_eq!(book.balances.len(), 1);
    assert_eq!(book.to_value(), zero);
    for invalid in [
        json!([]),
        json!({"version":2,"balances":[]}),
        json!({"version":1}),
        json!({"version":1,"balances":[],"other":1}),
        json!({"version":1,"balances":[{"currency":"EUR","amount":1}]}),
        json!({"version":1,"balances":[{"currency":"CNY","amount":0.001}]}),
        json!({"version":1,"balances":[{"currency":"CNY","amount":1,"other":1}]}),
        json!({"version":1,"balances":[{"currency":"CNY","amount":1},{"currency":"CNY","amount":2}]}),
    ] {
        assert!(Book::from_value(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn public_cent_sum_quantizes_each_item_before_summing_and_never_overflows() {
    let first = quantize(0.006).unwrap();
    assert_eq!(first, 1);
    assert_eq!(sum([first, first].into_iter()), Some((2, 0.02)));
    assert_eq!(quantize(0.004), Some(0));
    assert!(sum([u64::MAX, 1].into_iter()).is_none());
}
