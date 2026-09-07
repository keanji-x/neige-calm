// Shared `hq.sinajs.cn` fixture rows and response builder.
//
// `include!`d — not `mod`-ed — by BOTH test bodies that stand a loopback
// server in for that endpoint:
//
//   * `plugins/market/main.rs`'s own `#[cfg(test)]` module, and
//   * `crates/calm-server/tests/cases/market_plugin_process.rs`.
//
// They used to hold two independent copies of these rows, of the GBK name
// bytes and of the body builder. Two copies of a fixture that encodes a real
// endpoint's wire format drift apart one edit at a time, and the drift shows
// up as one suite passing while the other tests a format the source stopped
// using. There is one copy now.
//
// Everything here is test-only. It is `include!`d inside a `#[cfg(test)]`
// scope in the plugin and inside a test target in the process suite, so it is
// never compiled into a shipping binary.

/// 贵州癨 in GBK. The last character is `B0 5C` — a GBK character whose
/// TRAILING byte is the ASCII backslash — so a fixture row built with it
/// exercises the decode the parser depends on rather than a convenient
/// all-ASCII stand-in.
const SINA_FIXTURE_GBK_NAME: &[u8] = &[0xb9, 0xf3, 0xd6, 0xdd, 0xb0, 0x5c];

/// One live row per market, truncated after the fields this parser reads,
/// copied from a real `hq.sinajs.cn` response (2026-09-07).
///
/// The field orders are genuinely different — US is field 1, HK is field 6,
/// Shanghai and Shenzhen are field 3 — and the values are such that reading
/// another market's index out of one of these rows gives a DIFFERENT answer
/// rather than a coincidentally equal one: `gb_nvda`'s field 6 is 234.76,
/// `hk01810`'s field 3 is 28.440, and `sh600519` has no field 6 at all.
///
/// `<NAME>` stands for [`SINA_FIXTURE_GBK_NAME`]'s bytes.
const SINA_FIXTURE_ROWS: &[(&str, &str)] = &[
    (
        "gb_nvda",
        "<NAME>,230.3600,0.84,2026-09-05 09:46:13,1.9100,231.0900,234.7600,229.6300",
    ),
    (
        "hk01810",
        "XIAOMI-W,<NAME>,28.220,28.440,28.400,27.120,27.480,-0.960,-3.376",
    ),
    (
        "sh600519",
        "<NAME>,1324.000,1330.000,1316.940,1333.600,1312.660",
    ),
    (
        "sz000001",
        "<NAME>,11.870,11.890,11.700,11.880,11.650,11.690,11.700",
    ),
    (
        "sz300750",
        "<NAME>,351.050,351.000,348.200,351.800,345.500,348.190",
    ),
];

/// The exchange-rate rows, copied from the same live endpoint on the same day
/// (2026-09-07) and truncated after field 9 — the current rate is field 8 and
/// the pair's name is field 9, whose GBK bytes are kept so the FX path decodes
/// a real row rather than a convenient all-ASCII one.
///
/// **`fx_susdcny` is the row that discriminates.** Its field 1 (bid, 6.7099),
/// field 3 (previous close, 6.7108) and field 8 (current, 6.7111) are three
/// different numbers, so a parser reading any field but 8 gives a different
/// answer here. The others are weaker on purpose: they are what the endpoint
/// actually served, and on a spot-quoted pair the bid and the current rate
/// often coincide.
///
/// Sina quotes all six ordered pairs over USD, HKD and CNY natively, which is
/// why this plugin never divides one into another. All six are here even
/// though only four are routed today, so a route that reached for the wrong
/// direction gets the WRONG number out of this table rather than nothing at
/// all.
const SINA_FIXTURE_FX_ROWS: &[(&str, &str)] = &[
    (
        "fx_susdcny",
        "19:31:20,6.7099000000,6.7123000000,6.7108000000,139.0000000000,6.7103000000,6.7123000000,6.6984000000,6.7111000000,<NAME>",
    ),
    (
        "fx_scnyusd",
        "18:29:18,0.149007,0.149014,0.148994,0.8,0.149038,0.149038,0.148958,0.149007,<NAME>",
    ),
    (
        "fx_susdhkd",
        "19:31:25,7.839800,7.840500,7.840700,31,7.840700,7.841000,7.837900,7.839800,<NAME>",
    ),
    (
        "fx_shkdusd",
        "19:30:45,0.1275526474,0.1275526474,0.1275396329,0.5044180000,0.1275396329,0.1275851950,0.1275347532,0.1275526474,<NAME>",
    ),
    (
        "fx_shkdcny",
        "19:30:46,0.8560178052,0.8560178052,0.8560104776,5.1304190000,0.8560104776,0.8563623440,0.8558493021,0.8560178052,<NAME>",
    ),
    (
        "fx_scnyhkd",
        "19:31:25,1.168180,1.168340,1.168210,7,1.168210,1.168430,1.167730,1.168180,<NAME>",
    ),
];

/// Every row this fixture knows — the stock rows and the exchange-rate rows.
/// A test that prices a holding in one currency and settles in another needs
/// both from one server, so the default table is the union.
fn sina_fixture_all_rows() -> Vec<(&'static str, &'static str)> {
    SINA_FIXTURE_ROWS
        .iter()
        .chain(SINA_FIXTURE_FX_ROWS)
        .copied()
        .collect()
}

/// Build a Sina response for `target` out of a table of known rows, answering
/// every symbol the request asked for and no others.
///
/// A symbol the table does not know gets `""`, which is what the real endpoint
/// answers for a name it does not list (`gb_doge`, verified).
fn sina_fixture_body(target: &str, known: &[(&str, &str)]) -> Vec<u8> {
    let list = target.split("list=").nth(1).unwrap_or_default();
    let mut body = Vec::new();
    for symbol in list.split(',').filter(|s| !s.is_empty()) {
        let payload = known
            .iter()
            .find(|(name, _)| *name == symbol)
            .map_or("", |(_, payload)| *payload);
        body.extend_from_slice(format!("var hq_str_{symbol}=\"").as_bytes());
        for (index, chunk) in payload.split("<NAME>").enumerate() {
            if index > 0 {
                body.extend_from_slice(SINA_FIXTURE_GBK_NAME);
            }
            body.extend_from_slice(chunk.as_bytes());
        }
        body.extend_from_slice(b"\";\n");
    }
    body
}
