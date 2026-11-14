//! Canonical name codec and `--name` glob (#1838 S2).

use super::glob;
use crate::report_paths::name::{ParsedName, escape, parse, segments, unescape};

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
        let names = segments(&[(title, "0123456789abcdef")]);
        let file = &names[0];
        assert!(!file.contains('/') && !file.starts_with('.'), "{file:?}");
        let want_suffix = title.is_empty().then_some("01234567");
        assert_eq!(parse(file), Ok(parsed(title, want_suffix)), "{file:?}");
    }
}

#[test]
fn a_unique_title_lists_bare_and_an_empty_title_always_carries_its_suffix() {
    assert_eq!(
        segments(&[("认证 方案", "aaaa1111bbbb"), ("登录 排查", "aaaa1111cccc")]),
        vec!["认证 方案", "登录 排查"]
    );
    assert_eq!(segments(&[("", "abcdef0123456789")]), vec!["~abcdef01"]);
    let err = parse("").unwrap_err();
    assert!(err.contains("untitled name is listed as `~<id>`"), "{err}");
}

#[test]
fn shared_titles_all_take_the_shortest_unique_id_prefix_of_at_least_eight() {
    let names = segments(&[
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
            "认证 方案~0123456789aa",
            "登录 排查",
            "认证 方案~0123456789ab",
            "认证 方案~fedcba98",
            "~111122223",
            "~111122224",
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
    let names = segments(&[("a~01234567", "ffffffffffff"), ("a", "0123456789ab")]);
    assert_eq!(names, vec!["a%7E01234567", "a"]);
    assert_eq!(parse(&names[0]), Ok(parsed("a~01234567", None)));
    assert_eq!(parse("a~01234567"), Ok(parsed("a", Some("01234567"))));
}

#[test]
fn invalid_or_non_canonical_names_are_refused_not_guessed() {
    for bad in [
        "a%2fb", // lowercase hex
        "%41",   // an escape of a literal character
        "a%2",   // truncated
        "a%zz",  // not hex
        "%FF",   // not UTF-8
        "a~b~01234567",
        ".hidden", // unescaped leading dot
        ".",
        "a\\b",
        "a~",     // empty suffix
        "a~x%2F", // suffix outside the ID alphabet
    ] {
        assert!(parse(bad).is_err(), "{bad:?} must be refused");
    }
    for unknown in ["report", "report.txt"] {
        assert!(crate::report_paths::lookup(unknown, &[]).is_err());
    }
}

#[test]
fn glob_matches_star_and_question_mark_against_the_whole_name() {
    for (pattern, text, want) in [
        ("*认证*", "认证 方案", true),
        ("*认证*", "登录 排查", false),
        ("*", "", true),
        ("*", "a", true),
        ("?", "认", true),
        ("?", "ab", false),
        ("认证", "认证 方案", false),
        ("a*b*c", "aXXbYYc", true),
        ("a*b*c", "aXXbYY", false),
        ("[ab]", "[ab]", true),
        ("[ab]", "a", false),
        ("**x", "abx", true),
    ] {
        assert_eq!(
            glob::matches(pattern, text),
            want,
            "{pattern:?} vs {text:?}"
        );
    }
}
