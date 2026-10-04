//! The `area/reports/` name codec, path classification and `--name` glob (#1838 S2).

use super::name::{ParsedName, escape, file_names, parse, unescape};
use super::{AreaPath, classify, glob};

fn parsed(title: &str, suffix: Option<&str>) -> ParsedName {
    ParsedName {
        title: title.into(),
        suffix: suffix.map(str::to_string),
    }
}

#[test]
fn escape_keeps_ordinary_text_literal_and_encodes_exactly_the_special_characters() {
    for (title, name) in [
        ("认证 方案", "认证 方案"),
        ("认证 方案 第二版", "认证 方案 第二版"),
        (
            "a b!@#$^&*()[]{}?:;'\",<>=+|",
            "a b!@#$^&*()[]{}?:;'\",<>=+|",
        ),
        ("a/b", "a%2Fb"),
        ("100%", "100%25"),
        ("a~b", "a%7Eb"),
        ("~", "%7E"),
        ("a\\b", "a%5Cb"),
        ("line\nbreak\t\u{7f}", "line%0Abreak%09%7F"),
        ("\u{85}", "%C2%85"),
        (".hidden", "%2Ehidden"),
        (".", "%2E"),
        ("..", "%2E."),
        ("a.b.", "a.b."),
        ("v1.md", "v1.md"),
        ("", ""),
    ] {
        assert_eq!(escape(title), name, "{title:?}");
        assert_eq!(unescape(name).as_deref(), Ok(title), "{name:?}");
        let names = file_names(&[(title, "0123456789abcdef")]);
        let file = &names[0];
        assert!(!file.contains('/') && !file.starts_with('.'), "{file:?}");
        let want_suffix = title.is_empty().then_some("01234567");
        assert_eq!(parse(file), Ok(parsed(title, want_suffix)), "{file:?}");
    }
}

#[test]
fn a_unique_title_lists_bare_and_an_empty_title_always_carries_its_suffix() {
    assert_eq!(
        file_names(&[("认证 方案", "aaaa1111bbbb"), ("登录 排查", "aaaa1111cccc")]),
        vec!["认证 方案.md", "登录 排查.md"]
    );
    assert_eq!(
        file_names(&[("", "abcdef0123456789")]),
        vec!["~abcdef01.md"]
    );
    let err = parse(".md").unwrap_err();
    assert!(
        err.contains("untitled report is listed as `~<id>.md`"),
        "{err}"
    );
}

#[test]
fn shared_titles_all_take_the_shortest_unique_id_prefix_of_at_least_eight() {
    let names = file_names(&[
        ("认证 方案", "0123456789aaaa"),
        ("登录 排查", "0123456789bbbb"),
        ("认证 方案", "0123456789abcd"),
        ("认证 方案", "fedcba9876"),
        ("", "11112222333"),
        ("", "11112222444"),
    ]);
    assert_eq!(
        names,
        vec![
            "认证 方案~0123456789aa.md",
            "登录 排查.md",
            "认证 方案~0123456789ab.md",
            "认证 方案~fedcba98.md",
            "~111122223.md",
            "~111122224.md",
        ]
    );
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "every listed name is distinct");
    for (file, title, suffix) in [
        (&names[0], "认证 方案", "0123456789aa"),
        (&names[4], "", "111122223"),
    ] {
        assert_eq!(parse(file), Ok(parsed(title, Some(suffix))));
    }
}

#[test]
fn an_escaped_title_never_collides_with_a_generated_suffix() {
    // The title `a~01234567` would read as title `a` + suffix if `~` were literal.
    let names = file_names(&[("a~01234567", "ffffffffffff"), ("a", "0123456789ab")]);
    assert_eq!(names, vec!["a%7E01234567.md", "a.md"]);
    assert_eq!(parse(&names[0]), Ok(parsed("a~01234567", None)));
    assert_eq!(parse("a~01234567.md"), Ok(parsed("a", Some("01234567"))));
}

#[test]
fn invalid_or_non_canonical_names_are_refused_not_guessed() {
    for bad in [
        "a%2fb.md", // lowercase hex
        "%41.md",   // an escape of a literal character
        "a%2.md",   // truncated
        "a%zz.md",  // not hex
        "%FF.md",   // not UTF-8
        "a~b~01234567.md",
        ".hidden.md", // unescaped leading dot
        "..md",
        "a\\b.md",
        "a~.md",     // empty suffix
        "a~x%2F.md", // suffix outside the ID alphabet
        "report",    // no .md
        "report.txt",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn classify_routes_only_area_paths_and_refuses_traversal() {
    assert_eq!(classify("report.md"), None);
    assert_eq!(classify(""), None);
    assert_eq!(classify("areas/x"), None);
    assert_eq!(classify("area"), Some(Ok(AreaPath::Root)));
    assert_eq!(classify("area/reports"), Some(Ok(AreaPath::Reports)));
    assert_eq!(
        classify("area/reports/认证 方案 第二版.md"),
        Some(Ok(AreaPath::Report("认证 方案 第二版.md")))
    );
    for bad in [
        "area/reports/../x",
        "area/reports/a/b.md",
        "area/reports//x.md",
        "area/../report.md",
        "area//reports",
        "area/other",
        "area/reports/",
    ] {
        assert!(
            matches!(classify(bad), Some(Err(_))),
            "{bad:?}: {:?}",
            classify(bad)
        );
    }
}

#[test]
fn glob_matches_star_and_question_mark_against_the_whole_name() {
    for (pattern, text, want) in [
        ("*认证*", "认证 方案.md", true),
        ("*认证*", "登录 排查.md", false),
        ("*", "", true),
        ("*.md", "a.md", true),
        ("?.md", "认.md", true),
        ("?.md", "ab.md", false),
        ("认证", "认证 方案.md", false),
        ("a*b*c", "aXXbYYc", true),
        ("a*b*c", "aXXbYY", false),
        ("[ab].md", "[ab].md", true),
        ("[ab].md", "a.md", false),
        ("**x", "abx", true),
    ] {
        assert_eq!(
            glob::matches(pattern, text),
            want,
            "{pattern:?} vs {text:?}"
        );
    }
}
