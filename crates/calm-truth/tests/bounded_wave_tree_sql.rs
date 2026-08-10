//! Property gate for recursive SQL over `waves.parent_wave_id`.
//!
//! This deliberately does not maintain a declaration registry. It walks every
//! Rust token tree under this crate's `src/`, decodes string literals wherever
//! they occur (consts, statics, inline modules, blocks, and macro definitions
//! or invocations), strips SQL comments, and checks the dangerous property
//! itself.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};

fn rust_sources_below(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("read Rust source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            rust_sources_below(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// Return the decoded string-literal text for this group and every nested
/// group. Recording each nesting level means split `concat!("WITH ...",
/// "parent_wave_id ...")` text is checked as one expansion candidate, while
/// a complete SQL literal inside a wrapper macro is also checked on its own.
fn decoded_string_groups(stream: TokenStream, groups: &mut Vec<String>) -> String {
    let mut combined = String::new();
    for token in stream {
        match token {
            TokenTree::Literal(literal) => {
                if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                    let value = value.value();
                    groups.push(value.clone());
                    combined.push_str(&value);
                    combined.push('\n');
                }
            }
            TokenTree::Group(group) => {
                combined.push_str(&decoded_string_groups(group.stream(), groups));
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
    if !combined.trim().is_empty() {
        groups.push(combined.clone());
    }
    combined
}

fn sql_without_comments(sql: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Code,
        SingleQuote,
        DoubleQuote,
        LineComment,
        BlockComment,
    }

    let chars: Vec<char> = sql.chars().collect();
    let mut output = String::with_capacity(sql.len());
    let mut state = State::Code;
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();
        match state {
            State::Code if current == '-' && next == Some('-') => {
                state = State::LineComment;
                index += 2;
            }
            State::Code if current == '/' && next == Some('*') => {
                state = State::BlockComment;
                index += 2;
            }
            State::Code if current == '\'' => {
                state = State::SingleQuote;
                output.push(current);
                index += 1;
            }
            State::Code if current == '"' => {
                state = State::DoubleQuote;
                output.push(current);
                index += 1;
            }
            State::SingleQuote if current == '\'' && next == Some('\'') => {
                output.push_str("''");
                index += 2;
            }
            State::DoubleQuote if current == '"' && next == Some('"') => {
                output.push_str("\"\"");
                index += 2;
            }
            State::SingleQuote if current == '\'' => {
                state = State::Code;
                output.push(current);
                index += 1;
            }
            State::DoubleQuote if current == '"' => {
                state = State::Code;
                output.push(current);
                index += 1;
            }
            State::LineComment if current == '\n' => {
                state = State::Code;
                output.push('\n');
                index += 1;
            }
            State::BlockComment if current == '*' && next == Some('/') => {
                state = State::Code;
                output.push(' ');
                index += 2;
            }
            State::LineComment | State::BlockComment => index += 1,
            State::Code | State::SingleQuote | State::DoubleQuote => {
                output.push(current);
                index += 1;
            }
        }
    }
    output
}

fn sql_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let uncommented = sql_without_comments(sql);
    let mut chars = uncommented.chars().peekable();
    while let Some(character) = chars.next() {
        if character.is_ascii_alphanumeric() || character == '_' {
            word.push(character.to_ascii_lowercase());
            continue;
        }
        if !word.is_empty() {
            tokens.push(std::mem::take(&mut word));
        }
        if character.is_whitespace() {
            continue;
        }
        if character == '<' && chars.peek() == Some(&'=') {
            chars.next();
            tokens.push("<=".into());
        } else {
            tokens.push(character.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

fn recursive_parent_cte_is_bounded(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(where_index, token)| {
        if token != "where" {
            return false;
        }
        let predicate = &tokens[where_index + 1..tokens.len().min(where_index + 16)];
        predicate.iter().enumerate().any(|(depth_index, token)| {
            token == "depth"
                && predicate
                    .get(depth_index + 1)
                    .is_some_and(|operator| operator == "<" || operator == "<=")
        })
    })
}

fn unbounded_recursive_parent_ctes(source: &str) -> Vec<String> {
    let stream: TokenStream = source.parse().expect("parse Rust token stream");
    let mut groups = Vec::new();
    decoded_string_groups(stream, &mut groups);
    let mut violations = Vec::new();
    for group in groups {
        let tokens = sql_tokens(&group);
        for start in 0..tokens.len().saturating_sub(1) {
            if tokens[start] != "with" || tokens[start + 1] != "recursive" {
                continue;
            }
            let end = ((start + 2)..tokens.len().saturating_sub(1))
                .find(|index| tokens[*index] == "with" && tokens[*index + 1] == "recursive")
                .unwrap_or(tokens.len());
            let cte = &tokens[start..end];
            if cte.iter().any(|token| token == "parent_wave_id")
                && !recursive_parent_cte_is_bounded(cte)
            {
                violations.push(cte.join(" "));
            }
        }
    }
    violations.sort();
    violations.dedup();
    violations
}

#[test]
fn every_recursive_parent_wave_cte_in_the_crate_has_a_depth_bound() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources_below(&source_root, &mut files);
    files.sort();

    let mut failures = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("read Rust source");
        for violation in unbounded_recursive_parent_ctes(&source) {
            failures.push(format!("{}: {violation}", file.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "recursive parent-wave CTEs without a depth bound:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_property_gate_catches_every_previously_unseen_rust_shape() {
    let cases = [
        (
            "inline module",
            r##"mod hidden { pub const SQL: &str = r#"WITH RECURSIVE down(id) AS (SELECT id FROM waves UNION ALL SELECT w.id FROM waves w JOIN down ON w.parent_wave_id=down.id) SELECT id FROM down"#; }"##,
        ),
        (
            "wrapper macro",
            r##"macro_rules! wrap { ($sql:expr) => { $sql } } pub const SQL: &str = wrap!(r#"WITH RECURSIVE down(id) AS (SELECT id FROM waves UNION ALL SELECT w.id FROM waves w JOIN down ON w.parent_wave_id=down.id) SELECT id FROM down"#);"##,
        ),
        (
            "static",
            r##"pub static SQL: &str = r#"WITH RECURSIVE down(id) AS (SELECT id FROM waves UNION ALL SELECT w.id FROM waves w JOIN down ON w.parent_wave_id=down.id) SELECT id FROM down"#;"##,
        ),
        (
            "block expression",
            r##"pub const SQL: &str = { r#"WITH RECURSIVE down(id) AS (SELECT id FROM waves UNION ALL SELECT w.id FROM waves w JOIN down ON w.parent_wave_id=down.id) SELECT id FROM down"# };"##,
        ),
    ];
    for (shape, source) in cases {
        assert!(
            !unbounded_recursive_parent_ctes(source).is_empty(),
            "{shape} bypassed the property gate"
        );
    }
}

#[test]
fn comments_cannot_supply_the_depth_predicate() {
    let source = r##"pub const SQL: &str = r#"
        WITH RECURSIVE down(id) AS (
          SELECT id FROM waves
          UNION ALL
          SELECT w.id FROM waves w JOIN down ON w.parent_wave_id = down.id
          /* WHERE down.depth <= ?2 */
        ) SELECT id FROM down
    "#;"##;
    assert!(!unbounded_recursive_parent_ctes(source).is_empty());
}

#[test]
fn formatting_and_sql_comments_do_not_hide_a_real_depth_predicate() {
    let source = r##"pub static SQL: &str = r#"
        WITH /* layout */ RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
          WHERE
            down . depth
            <=
            ?2
        ) SELECT id FROM down
    "#;"##;
    assert!(unbounded_recursive_parent_ctes(source).is_empty());
}
