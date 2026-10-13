use super::*;

const EXAMPLE: &str = include_str!("../../research-archive.example.toml");

/// The example with the line starting `field =` replaced by `line` (or
/// removed when `line` is empty).
fn with(field: &str, line: &str) -> String {
    let prefix = format!("{field} =");
    let mut found = false;
    let out: Vec<&str> = EXAMPLE
        .lines()
        .filter_map(|l| {
            if l.starts_with(&prefix) {
                found = true;
                (!line.is_empty()).then_some(line)
            } else {
                Some(l)
            }
        })
        .collect();
    assert!(found, "the example has no {field}");
    out.join("\n")
}

fn refused(text: &str) -> ConfigError {
    Config::parse(text).expect_err("config must be refused")
}

fn field_of(err: &ConfigError) -> &'static str {
    match err {
        ConfigError::Invalid { field, .. } => field,
        other => panic!("expected a field error, got {other}"),
    }
}

#[test]
fn the_example_parses_into_the_typed_config() {
    let config = Config::parse(EXAMPLE).unwrap();
    assert_eq!(
        config.data_dir,
        PathBuf::from("/home/user/.local/share/neige-next/research-archive/data")
    );
    assert_eq!(
        config.oauth_client_id.as_str(),
        "replace-with-the-client-id"
    );
    assert_eq!(config.access_region, AccessRegion::Cn);
    assert_eq!(config.http_url, "https://openapi.longbridge.cn");
    assert_eq!(config.quote_ws_url, "wss://openapi-quote.longbridge.cn/v2");
    assert_eq!(config.callback_port.get(), 60355);
    let research: Vec<_> = config.research.iter().map(Key::as_str).collect();
    assert_eq!(research, ["AAPL.US", "MSFT.US", "NVDA.US"]);
    let indexes: Vec<_> = config.indexes.iter().map(Key::as_str).collect();
    assert_eq!(indexes, [".SPX.US"]);
    assert_eq!(config.rps.get(), 2);
    assert_eq!(config.attempt_cap, 6);
    assert_eq!(config.attempt_timeout, Duration::from_secs(25 * 60));
    assert_eq!(config.carry_deadline, Duration::from_secs(60 * 60));
    assert_eq!(
        config.window_start,
        NaiveTime::from_hms_opt(17, 30, 0).unwrap()
    );
    assert_eq!(config.notify_command[0], "/usr/local/bin/notify-owner");
    assert_eq!(
        config.archive_path(),
        config.data_dir.join("archive.sqlite3")
    );
    assert_eq!(config.oauth_dir(), config.data_dir.join("oauth"));
}

#[test]
fn an_unknown_key_is_refused() {
    let text = format!("{EXAMPLE}\nrps_default = 2\n");
    assert!(matches!(refused(&text), ConfigError::Parse { .. }));
}

#[test]
fn every_field_is_required() {
    for field in [
        "data_dir",
        "oauth_client_id",
        "access_region",
        "http_url",
        "quote_ws_url",
        "callback_port",
        "research",
        "indexes",
        "rps",
        "attempt_cap",
        "attempt_timeout_minutes",
        "carry_deadline_minutes",
        "window_start",
        "notify_command",
    ] {
        match refused(&with(field, "")) {
            ConfigError::Parse { message, .. } => {
                assert!(message.contains(field), "{field}: {message}");
            }
            other => panic!("{field}: {other}"),
        }
    }
}

#[test]
fn keys_must_have_their_form_and_be_unique() {
    for (field, line) in [
        ("indexes", r#"indexes = ["SPX.US"]"#),
        ("indexes", r#"indexes = [".spx.US"]"#),
        ("indexes", r#"indexes = [".SPX.US", ".SPX.US"]"#),
        ("research", r#"research = [".AAPL.US"]"#),
        ("research", r#"research = ["aapl.US"]"#),
        ("research", r#"research = ["AAPL"]"#),
        ("research", r#"research = ["AAPL.USA"]"#),
        ("research", r#"research = ["AAPL .US"]"#),
        ("research", r#"research = ["AAPL..US"]"#),
        ("research", r#"research = ["AAPL.US", "AAPL.US"]"#),
    ] {
        assert_eq!(field_of(&refused(&with(field, line))), field, "{line}");
    }
    let both_empty =
        with("research", "research = []").replace(r#"indexes = [".SPX.US"]"#, "indexes = []");
    assert_eq!(field_of(&refused(&both_empty)), "research");
    for ok in [
        r#"research = ["BRK.B.US", "700.HK", "600519.SH"]"#,
        "research = []",
    ] {
        Config::parse(&with("research", ok)).unwrap();
    }
}

#[test]
fn urls_need_their_scheme_and_shape() {
    for (field, line) in [
        ("http_url", r#"http_url = "http://openapi.longbridge.cn""#),
        // Plain http is for a loopback IP literal only.
        ("http_url", r#"http_url = "http://localhost:8080""#),
        ("http_url", r#"http_url = "http://10.0.0.1:8080""#),
        ("http_url", r#"http_url = "http://127.0.0.1:8080/""#),
        ("http_url", r#"http_url = "https://openapi.longbridge.cn/""#),
        (
            "http_url",
            r#"http_url = "https://openapi.longbridge.cn/v1""#,
        ),
        (
            "http_url",
            r#"http_url = "https://openapi.longbridge.cn?x=1""#,
        ),
        (
            "http_url",
            r#"http_url = "https://u:p@openapi.longbridge.cn""#,
        ),
        ("http_url", r#"http_url = "openapi.longbridge.cn""#),
        (
            "quote_ws_url",
            r#"quote_ws_url = "ws://openapi-quote.longbridge.cn/v2""#,
        ),
        (
            "quote_ws_url",
            r#"quote_ws_url = "https://openapi-quote.longbridge.cn/v2""#,
        ),
    ] {
        assert_eq!(field_of(&refused(&with(field, line))), field, "{line}");
    }
    Config::parse(&with("http_url", r#"http_url = "https://127.0.0.1:8443""#)).unwrap();
    Config::parse(&with("http_url", r#"http_url = "http://127.0.0.1:8080""#)).unwrap();
    Config::parse(&with("http_url", r#"http_url = "http://[::1]:8080""#)).unwrap();
}

#[test]
fn scalar_fields_are_range_checked() {
    for (field, line) in [
        ("data_dir", r#"data_dir = "relative/dir""#),
        ("oauth_client_id", r#"oauth_client_id = "../x""#),
        ("callback_port", "callback_port = 0"),
        ("rps", "rps = 0"),
        ("rps", "rps = 21"),
        ("attempt_cap", "attempt_cap = 0"),
        ("attempt_timeout_minutes", "attempt_timeout_minutes = 0"),
        ("carry_deadline_minutes", "carry_deadline_minutes = 601"),
        ("window_start", r#"window_start = "9:30""#),
        ("window_start", r#"window_start = "15:59""#),
        ("window_start", r#"window_start = "17:30:00""#),
        ("notify_command", "notify_command = []"),
        ("notify_command", r#"notify_command = ["notify-owner"]"#),
    ] {
        assert_eq!(field_of(&refused(&with(field, line))), field, "{line}");
    }
    assert!(matches!(
        refused(&with("access_region", r#"access_region = "hk""#)),
        ConfigError::Parse { .. }
    ));
}

/// The carry deadline must fall inside the shortest window the start time
/// allows: 17:30 to 09:30 the next day is 16 hours, 15 across the spring
/// DST change; 23:59 to 09:30 is 9 h 31 min, 8 h 31 min (511 minutes).
#[test]
fn the_carry_deadline_ends_before_the_shortest_window() {
    let at = |ws: &str, carry: u32| {
        with("window_start", &format!("window_start = \"{ws}\"")).replace(
            "carry_deadline_minutes = 60",
            &format!("carry_deadline_minutes = {carry}"),
        )
    };
    assert_eq!(
        shortest_window_minutes(NaiveTime::from_hms_opt(17, 30, 0).unwrap()),
        15 * 60
    );
    Config::parse(&at("17:30", 600)).unwrap();
    Config::parse(&at("23:59", 510)).unwrap();
    for (ws, carry) in [("23:59", 511), ("23:59", 600)] {
        let err = refused(&at(ws, carry));
        assert_eq!(field_of(&err), "carry_deadline_minutes", "{ws} {carry}");
        assert!(err.to_string().contains("at most 510"), "{err}");
    }
}
