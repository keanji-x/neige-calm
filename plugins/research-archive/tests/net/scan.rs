//! A source scan: test code builds SDK clients only under `tests/net/`,
//! where [`crate::hermetic`] pins the environment. Elsewhere a regression
//! could make the SDK probe `geotest.lbkrs.com` or reach the OAuth host.
//!
//! **What is test code.** Every `.rs` file under `tests/` except
//! `tests/net/`; under `src/`, every `tests.rs` file and every item marked
//! `#[cfg(test)]` (an inline `mod tests { … }`). Production code is not
//! scanned: it builds SDK clients by design.
//!
//! **The one exemption.** A comment line (`//`, `///`, `//!`) may name an
//! SDK item: prose builds nothing. Every code line is checked.

use std::path::{Path, PathBuf};

/// What names or builds an SDK client, or reaches the SDK at all.
const PATTERNS: [&str; 11] = [
    "longbridge::",
    "use longbridge",
    "auth::login(",
    "build_oauth(",
    "OAuthBuilder",
    "OAuth::from_token(",
    "LongbridgeSource::new(",
    "HttpClient::new(",
    "HttpClientConfig::",
    "QuoteContext::new",
    "Config::from_oauth",
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

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// A scanned file and its `(line number, line)` pairs.
type Scanned = (PathBuf, Vec<(usize, String)>);

/// Every scanned file of the package, with the counts of test files and
/// inline `#[cfg(test)]` items found.
fn test_code(package: &Path) -> (Vec<Scanned>, usize, usize) {
    let whole = |path: &Path| -> Vec<(usize, String)> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .enumerate()
            .map(|(n, l)| (n + 1, l.to_owned()))
            .collect()
    };
    let mut test_files = Vec::new();
    rust_files(&package.join("tests"), &mut test_files);
    test_files.retain(|p| !p.starts_with(package.join("tests/net")));
    let mut scanned: Vec<_> = test_files.iter().map(|p| (p.clone(), whole(p))).collect();

    let mut src_files = Vec::new();
    rust_files(&package.join("src"), &mut src_files);
    let mut inline_items = 0;
    for path in &src_files {
        if path.file_name().is_some_and(|n| n == "tests.rs") {
            scanned.push((path.clone(), whole(path)));
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for item in cfg_test_items(&text) {
            inline_items += 1;
            let lines = item.into_iter().map(|(n, l)| (n, l.to_owned())).collect();
            scanned.push((path.clone(), lines));
        }
    }
    (scanned, test_files.len(), inline_items)
}

#[test]
fn sdk_clients_are_built_only_under_the_hermetic_harness() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (scanned, test_files, inline_items) = test_code(package);
    assert!(test_files >= 10, "the scan found the test files");
    assert!(inline_items >= 8, "the scan found the inline test items");

    let mut escaped = Vec::new();
    for (path, lines) in &scanned {
        let relative = path.strip_prefix(package).unwrap();
        for (n, line) in lines {
            if is_comment(line) {
                continue;
            }
            for pattern in PATTERNS {
                if line.contains(pattern) {
                    escaped.push(format!("{}:{n}: {pattern}", relative.display()));
                }
            }
        }
    }
    assert!(escaped.is_empty(), "outside tests/net: {escaped:#?}");
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
