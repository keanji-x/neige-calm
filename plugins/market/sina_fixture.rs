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
