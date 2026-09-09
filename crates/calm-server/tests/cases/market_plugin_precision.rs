//! Quotes and holdings must describe the same unit price; values still use cents.
use super::*;

fn etf_source() -> String {
    sina_server_with_rows(vec![(
        "sh510300",
        "Sample ETF,4.630,4.620,4.637,4.640,4.610",
    )])
}

#[test]
fn quoted_price_precision_survives_complete_holdings_and_overlay() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &etf_source(), 3600, "CNY");
    let quote = kernel.call_tool(2, "market.quote", json!({"asset":"SH:510300"}), Some(TRACK));
    assert_eq!(quote["result"]["structuredContent"]["price"], 4.637);
    kernel.set_holding(3, "SH:510300", 7.0, TRACK);
    let table = last_holdings_table(&kernel, TRACK);
    assert_eq!(
        table["rows"][0]["price"],
        quote["result"]["structuredContent"]["price"]
    );
    assert_eq!(
        table["rows"][0]["value"], 32.46,
        "unit precision must not change cent-valued amounts"
    );
    assert_eq!(table["rows"][1]["value"], 32.46);
    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    assert_eq!(
        listed["result"]["structuredContent"]["holdings"][0]["price"],
        4.637
    );
    assert_eq!(listed["result"]["structuredContent"]["total"], 32.46);
}

#[test]
fn quoted_price_precision_survives_missing_fx_without_inventing_value() {
    // This endpoint has a CNY quote but no FX rows, so USD settlement fails.
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &etf_source(), 3600, "USD");
    let quote = kernel.call_tool(2, "market.quote", json!({"asset":"SH:510300"}), Some(TRACK));
    assert_eq!(quote["result"]["structuredContent"]["price"], 4.637);
    kernel.set_holding(3, "SH:510300", 7.0, TRACK);
    let table = last_holdings_table(&kernel, TRACK);
    assert_eq!(
        table["rows"][0]["price"],
        quote["result"]["structuredContent"]["price"]
    );
    assert_eq!(table["rows"][0]["currency"], "CNY");
    assert!(table["rows"][0]["value"].is_null());
    assert!(table["rows"][1]["value"].is_null());
    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    assert_eq!(
        listed["result"]["structuredContent"]["holdings"][0]["price"],
        4.637
    );
    assert_eq!(listed["result"]["structuredContent"]["complete"], false);
    assert!(listed["result"]["structuredContent"]["total"].is_null());
}
