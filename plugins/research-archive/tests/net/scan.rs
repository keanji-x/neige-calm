//! A source scan: test code builds SDK clients only under `tests/net/`,
//! where [`crate::hermetic`] pins the environment. Elsewhere a regression
//! could make the SDK probe `geotest.lbkrs.com` or reach the OAuth host.
//!
//! It catches accidental placement of such a test outside the harness. It
//! is not a defence against deliberate evasion.
//!
//! **What is test code.** Every `.rs` file under `tests/` except
//! `tests/net/`; under `src/`, every `tests.rs` file and every item marked
//! `#[cfg(test)]` (an inline `mod tests { … }`). Production code is not
//! scanned: it builds SDK clients by design.
//!
//! **How.** Each pattern is a regex over the whole text of a file or item,
//! so a call split across lines still matches. Comments are scanned too:
//! prose that names an SDK item is reworded rather than exempted.

use std::path::{Path, PathBuf};

use regex::Regex;

/// What names or builds an SDK client, or reaches the SDK at all.
const PATTERNS: [&str; 7] = [
    // Calls, qualified or not (`use …::auth::login` or `auth::*` first).
    r"\b(login|build_oauth|from_token)\s*\(",
    // SDK client types and the archive's source, as whole words.
    r"\b(OAuthBuilder|LongbridgeSource|HttpClient|HttpClientConfig|QuoteContext)\b",
    // Any path into the SDK crate, or an import of it.
    r"\blongbridge::|\buse\s+longbridge\b",
    // A glob or `login` import from `auth`.
    r"\bauth::(\*|\{[^}]*\blogin\b|login\b)",
    // The SDK's own config constructors.
    r"\bConfig::from_oauth\b",
    // A capture start, which builds the source and the OAuth client: a
    // call, a glob, or an import of `run`/`run_with` from `capture`.
    r"\bcapture::(\*|\{[^}]*\brun(_with)?\b|run(_with)?\s*\()",
    // The calendar's SDK source, which builds a quote connection.
    r"\bQuoteDays\b",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The `(line number, line)` pairs of each `#[cfg(test)]` item in `text`:
/// from the attribute to the end of the item, matched by braces, or to the
/// first `;` for an item without a body (`mod tests;`, whose file is
/// scanned as a `tests.rs`).
fn cfg_test_items(text: &str) -> Vec<Vec<(usize, &str)>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut items = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        let mut item = Vec::new();
        let mut depth = 0i64;
        let mut opened = false;
        let mut j = i;
        while j < lines.len() {
            let line = lines[j];
            item.push((j + 1, line));
            depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
            opened |= line.contains('{');
            j += 1;
            let bodiless = !opened && j > i + 1 && line.trim_end().ends_with(';');
            if (opened && depth <= 0) || bodiless {
                break;
            }
        }
        items.push(item);
        i = j;
    }
    items
}

/// A scanned unit: a file, or one `#[cfg(test)]` item, with the number of
/// its first line and its text.
type Scanned = (PathBuf, usize, String);

/// Every scanned unit of the package, with the counts of test files and
/// inline `#[cfg(test)]` items found.
fn test_code(package: &Path) -> (Vec<Scanned>, usize, usize) {
    let whole =
        |path: &Path| -> Scanned { (path.to_owned(), 1, std::fs::read_to_string(path).unwrap()) };
    let mut test_files = Vec::new();
    rust_files(&package.join("tests"), &mut test_files);
    test_files.retain(|p| !p.starts_with(package.join("tests/net")));
    let mut scanned: Vec<Scanned> = test_files.iter().map(|p| whole(p)).collect();

    let mut src_files = Vec::new();
    rust_files(&package.join("src"), &mut src_files);
    let mut inline_items = 0;
    for path in &src_files {
        if path.file_name().is_some_and(|n| n == "tests.rs") {
            scanned.push(whole(path));
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for item in cfg_test_items(&text) {
            inline_items += 1;
            let first = item[0].0;
            let lines: Vec<&str> = item.iter().map(|(_, l)| *l).collect();
            scanned.push((path.clone(), first, lines.join("\n")));
        }
    }
    (scanned, test_files.len(), inline_items)
}

/// `file:line: match` for every pattern match in the units.
fn matches(scanned: &[Scanned], package: &Path) -> Vec<String> {
    let patterns: Vec<Regex> = PATTERNS.iter().map(|p| Regex::new(p).unwrap()).collect();
    let mut found = Vec::new();
    for (path, first, text) in scanned {
        let relative = path.strip_prefix(package).unwrap_or(path);
        for pattern in &patterns {
            for m in pattern.find_iter(text) {
                let line = first + text[..m.start()].matches('\n').count();
                found.push(format!("{}:{line}: {}", relative.display(), m.as_str()));
            }
        }
    }
    found
}

#[test]
fn sdk_clients_are_built_only_under_the_hermetic_harness() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (scanned, test_files, inline_items) = test_code(package);
    assert!(test_files >= 10, "the scan found the test files");
    assert!(inline_items >= 8, "the scan found the inline test items");
    let escaped = matches(&scanned, package);
    assert!(escaped.is_empty(), "outside tests/net: {escaped:#?}");
}

/// Each form the scan must catch, and some it must not.
#[test]
fn the_patterns_catch_each_form() {
    let caught = [
        "let _ = login(&c, s, p, |_| {}, d);",
        "auth::login(c)",
        "build_oauth\n    (c, s, d)",
        "OAuth::from_token(\"x\")",
        "let s: LongbridgeSource = todo!();",
        "HttpClient::new(c)",
        "HttpClientConfig::from_oauth(o)",
        "QuoteContext::try_new(c)",
        "use longbridge::oauth::OAuth;",
        "use research_archive::auth::*;",
        "use research_archive::auth::{ClientId, login};",
        "Config::from_oauth(o)",
        "/*\n// */ auth::login(c)",
        "capture::run(&config, clock, None)",
        "research_archive::capture::run_with (&c, k, &d, None)",
        "use research_archive::capture::{MAX_IN_FLIGHT, run};",
        "use research_archive::capture::*;",
        "let days = QuoteDays;",
    ];
    for text in caught {
        let unit = (PathBuf::from("x.rs"), 1, text.to_owned());
        assert!(!matches(&[unit], Path::new("")).is_empty(), "{text:?}");
    }
    for text in [
        r#"env.run(&["auth", "login"])"#,
        "fn logins() {}",
        "capture::MAX_IN_FLIGHT",
        "capture::AUTH_BOUND",
        r#"env.run(&["capture"])"#,
    ] {
        let unit = (PathBuf::from("x.rs"), 1, text.to_owned());
        assert!(matches(&[unit], Path::new("")).is_empty(), "{text:?}");
    }
}

/// The item extraction itself: an inline module to its closing brace, and
/// an external `mod more;` alone.
#[test]
fn cfg_test_items_are_extracted_whole() {
    let text = "fn production() {}\n#[cfg(test)]\nmod tests {\n    fn a() {\n        x();\n    }\n}\nfn after() {}\n#[cfg(test)]\nmod more;\n";
    let numbers: Vec<Vec<usize>> = cfg_test_items(text)
        .iter()
        .map(|item| item.iter().map(|(n, _)| *n).collect())
        .collect();
    assert_eq!(numbers, [vec![2, 3, 4, 5, 6, 7], vec![9, 10]]);
}
