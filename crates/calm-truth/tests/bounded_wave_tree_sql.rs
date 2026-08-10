//! Property gate for recursive SQL over `waves.parent_wave_id`.
//!
//! This deliberately does not maintain a declaration registry. It scans the
//! production Rust and SQL sources of every crate that executes wave-tree SQL,
//! decodes each Rust string literal independently, strips SQL comments, and
//! checks the dangerous property itself: a recursive member which traverses
//! `parent_wave_id` must upper-bound the recursive CTE alias's own `depth` in
//! that member's ON/WHERE predicate.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};

fn production_sources_below(path: &Path, include_rust: bool, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }
    for entry in fs::read_dir(path).expect("read production source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            production_sources_below(&path, include_rust, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "sql" || (include_rust && extension == "rs"))
        {
            files.push(path);
        }
    }
}

fn literal_concat(stream: TokenStream) -> Option<String> {
    let tokens = stream.into_iter().collect::<Vec<_>>();
    let mut combined = String::new();
    let mut index = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            TokenTree::Literal(literal) => {
                combined.push_str(
                    &syn::parse_str::<syn::LitStr>(&literal.to_string())
                        .ok()?
                        .value(),
                );
                index += 1;
            }
            TokenTree::Punct(punct) if punct.as_char() == ',' => index += 1,
            TokenTree::Ident(ident)
                if ident == "concat"
                    && tokens.get(index + 1).is_some_and(
                        |token| matches!(token, TokenTree::Punct(p) if p.as_char() == '!'),
                    ) =>
            {
                let TokenTree::Group(group) = tokens.get(index + 2)? else {
                    return None;
                };
                combined.push_str(&literal_concat(group.stream())?);
                index += 3;
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Group(_) => return None,
        }
    }
    Some(combined)
}

/// Decode each literal separately, plus the expansion of literal-only
/// `concat!` invocations. Combining an entire token group creates a false SQL
/// program (unrelated consts in one file donate tokens to each other), while
/// ignoring a real `concat!("WITH ...", "parent_wave_id ...")` would create a
/// false negative. Scoping concatenation to the macro invocation buys both.
fn decoded_string_literals(stream: TokenStream, literals: &mut Vec<String>) {
    let tokens = stream.into_iter().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            TokenTree::Literal(literal) => {
                if let Ok(value) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                    literals.push(value.value());
                }
                index += 1;
            }
            TokenTree::Ident(ident)
                if ident == "concat"
                    && tokens.get(index + 1).is_some_and(
                        |token| matches!(token, TokenTree::Punct(p) if p.as_char() == '!'),
                    ) =>
            {
                if let Some(TokenTree::Group(group)) = tokens.get(index + 2) {
                    if let Some(combined) = literal_concat(group.stream()) {
                        literals.push(combined);
                    }
                    decoded_string_literals(group.stream(), literals);
                    index += 3;
                } else {
                    index += 1;
                }
            }
            TokenTree::Group(group) => {
                decoded_string_literals(group.stream(), literals);
                index += 1;
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) => index += 1,
        }
    }
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
        if matches!(character, '<' | '>') && chars.peek() == Some(&'=') {
            chars.next();
            tokens.push(format!("{character}="));
        } else {
            tokens.push(character.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

fn matching_right_paren(tokens: &[String], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.as_str() {
            "(" => depth += 1,
            ")" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_identifier(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn recursive_members(body: &[String]) -> Vec<&[String]> {
    let mut members = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, token) in body.iter().enumerate() {
        match token.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "union" if depth == 0 => {
                members.push(&body[start..index]);
                start = index + 1;
                if body.get(start).is_some_and(|token| token == "all") {
                    start += 1;
                }
            }
            _ => {}
        }
    }
    members.push(&body[start..]);
    members
}

fn recursive_aliases(member: &[String], cte_name: &str) -> BTreeSet<String> {
    let mut aliases = BTreeSet::new();
    for (index, token) in member.iter().enumerate() {
        if token != cte_name
            || !index
                .checked_sub(1)
                .is_some_and(|previous| matches!(member[previous].as_str(), "from" | "join" | ","))
        {
            continue;
        }
        aliases.insert(cte_name.to_owned());
        let mut alias_index = index + 1;
        if member.get(alias_index).is_some_and(|token| token == "as") {
            alias_index += 1;
        }
        if member.get(alias_index).is_some_and(|token| {
            is_identifier(token)
                && !matches!(
                    token.as_str(),
                    "on" | "where" | "join" | "left" | "right" | "inner" | "outer" | "cross"
                )
        }) {
            aliases.insert(member[alias_index].clone());
        }
    }
    aliases
}

fn recursive_depth_at(tokens: &[String], index: usize, aliases: &BTreeSet<String>) -> bool {
    tokens
        .get(index)
        .is_some_and(|token| aliases.contains(token))
        && tokens.get(index + 1).is_some_and(|token| token == ".")
        && tokens.get(index + 2).is_some_and(|token| token == "depth")
}

fn enclosing_condition_start(tokens: &[String], index: usize) -> Option<usize> {
    let clause = tokens[..index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, token)| {
            matches!(
                token.as_str(),
                "select"
                    | "from"
                    | "join"
                    | "on"
                    | "where"
                    | "group"
                    | "having"
                    | "order"
                    | "limit"
                    | "returning"
            )
        })?;
    matches!(clause.1.as_str(), "on" | "where").then_some(clause.0)
}

fn predicate_bounds_recursive_depth(member: &[String], aliases: &BTreeSet<String>) -> bool {
    for index in 0..member.len() {
        if !recursive_depth_at(member, index, aliases) {
            continue;
        }
        let Some(condition) = enclosing_condition_start(member, index) else {
            continue;
        };

        // recursive_alias.depth <= upper_bound
        if member
            .get(index + 3)
            .is_some_and(|operator| operator == "<" || operator == "<=")
        {
            let rhs_start = (index + 4).min(member.len());
            let rhs_end = member[rhs_start..]
                .iter()
                .position(|token| {
                    matches!(
                        token.as_str(),
                        "and" | "or" | "group" | "having" | "order" | "limit" | "returning"
                    )
                })
                .map_or(member.len(), |offset| rhs_start + offset);
            let rhs = &member[rhs_start..rhs_end];
            if !rhs.is_empty()
                && !(0..rhs.len()).any(|rhs_index| recursive_depth_at(rhs, rhs_index, aliases))
            {
                return true;
            }
        }

        // upper_bound >= recursive_alias.depth
        if index > 0
            && matches!(member[index - 1].as_str(), ">" | ">=")
            && condition + 1 < index - 1
        {
            let lhs_start = member[condition + 1..index - 1]
                .iter()
                .rposition(|token| matches!(token.as_str(), "and" | "or"))
                .map_or(condition + 1, |offset| condition + 2 + offset);
            let lhs = &member[lhs_start..index - 1];
            if !(0..lhs.len()).any(|lhs_index| recursive_depth_at(lhs, lhs_index, aliases)) {
                return true;
            }
        }
    }
    false
}

fn cte_body_is_unbounded(cte_name: &str, body: &[String]) -> bool {
    recursive_members(body).into_iter().any(|member| {
        let aliases = recursive_aliases(member, cte_name);
        !aliases.is_empty()
            && member.iter().any(|token| token == "parent_wave_id")
            && !predicate_bounds_recursive_depth(member, &aliases)
    })
}

fn unbounded_recursive_parent_ctes_in_sql(sql: &str) -> Vec<String> {
    let tokens = sql_tokens(sql);
    let mut violations = Vec::new();
    for with_index in 0..tokens.len() {
        if tokens[with_index] != "with" {
            continue;
        }
        let mut cursor = with_index + 1;
        if tokens.get(cursor).is_some_and(|token| token == "recursive") {
            cursor += 1;
        }
        while tokens.get(cursor).is_some_and(|token| is_identifier(token)) {
            let cte_name = tokens[cursor].clone();
            cursor += 1;
            if tokens.get(cursor).is_some_and(|token| token == "(") {
                let Some(close) = matching_right_paren(&tokens, cursor) else {
                    break;
                };
                cursor = close + 1;
            }
            if tokens.get(cursor).is_none_or(|token| token != "as") {
                break;
            }
            cursor += 1;
            if tokens.get(cursor).is_some_and(|token| token == "not") {
                cursor += 1;
            }
            if tokens
                .get(cursor)
                .is_some_and(|token| token == "materialized")
            {
                cursor += 1;
            }
            if tokens.get(cursor).is_none_or(|token| token != "(") {
                break;
            }
            let open = cursor;
            let Some(close) = matching_right_paren(&tokens, open) else {
                break;
            };
            let body = &tokens[open + 1..close];
            if cte_body_is_unbounded(&cte_name, body) {
                violations.push(tokens[with_index..=close].join(" "));
            }
            cursor = close + 1;
            if tokens.get(cursor).is_none_or(|token| token != ",") {
                break;
            }
            cursor += 1;
        }
    }
    violations.sort();
    violations.dedup();
    violations
}

fn unbounded_recursive_parent_ctes(source: &str) -> Vec<String> {
    let stream: TokenStream = source.parse().expect("parse Rust token stream");
    let mut literals = Vec::new();
    decoded_string_literals(stream, &mut literals);
    literals
        .into_iter()
        .flat_map(|literal| unbounded_recursive_parent_ctes_in_sql(&literal))
        .collect()
}

#[test]
fn every_recursive_parent_wave_cte_in_production_crates_bounds_its_recursive_variable() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = ["calm-truth", "calm-server"];
    let mut files = Vec::new();
    for crate_name in crates {
        let crate_root = workspace.join("crates").join(crate_name);
        production_sources_below(&crate_root.join("src"), true, &mut files);
        // `include_str!`, `query_file!`, and migrations commonly keep SQL
        // outside `src`; scan every .sql file in the executing crate.
        production_sources_below(&crate_root, false, &mut files);
    }
    files.sort();
    files.dedup();

    let mut failures = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("read production source");
        let violations = if file.extension().is_some_and(|extension| extension == "rs") {
            unbounded_recursive_parent_ctes(&source)
        } else {
            unbounded_recursive_parent_ctes_in_sql(&source)
        };
        for violation in violations {
            failures.push(format!("{}: {violation}", file.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "recursive parent-wave CTE members without a bound on their recursive depth variable:\n{}",
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
fn formatting_and_sql_comments_do_not_hide_a_real_recursive_depth_predicate() {
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

#[test]
fn outer_depth_filter_cannot_bound_the_recursive_member() {
    let sql = r#"
        WITH RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
        ) SELECT id FROM down WHERE depth <= ?2
    "#;
    assert!(!unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn sqlite_recursion_without_the_recursive_keyword_is_still_checked() {
    let sql = r#"
        WITH down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
        ) SELECT id FROM down
    "#;
    assert!(!unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn a_bound_on_an_unrelated_depth_does_not_protect_the_recursive_variable() {
    let sql = r#"
        WITH RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
          CROSS JOIN (SELECT 0 AS depth) guard
          WHERE guard.depth <= ?2
        ) SELECT id FROM down
    "#;
    assert!(!unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn a_depth_comparison_outside_an_on_or_where_predicate_is_not_a_bound() {
    let sql = r#"
        WITH RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
          ORDER BY down.depth <= ?2
        ) SELECT id FROM down
    "#;
    assert!(!unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn reversed_upper_bound_on_the_recursive_alias_is_valid() {
    let sql = r#"
        WITH RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
          WHERE ?2 >= down.depth
        ) SELECT id FROM down
    "#;
    assert!(unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn another_boolean_term_cannot_turn_a_real_bound_into_a_false_positive() {
    let sql = r#"
        WITH RECURSIVE down(id, depth) AS (
          SELECT id, 0 FROM waves
          UNION ALL
          SELECT w.id, down.depth + 1
          FROM waves w JOIN down ON w.parent_wave_id = down.id
          WHERE down.depth <= ?2 AND down.depth >= 0
        ) SELECT id FROM down
    "#;
    assert!(unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn an_unrelated_recursive_cte_does_not_borrow_parent_wave_usage() {
    let sql = r#"
        WITH RECURSIVE numbers(n) AS (
          SELECT 0 UNION ALL SELECT n + 1 FROM numbers WHERE n < 3
        ), roots AS (
          SELECT parent_wave_id FROM waves
        ) SELECT * FROM numbers, roots
    "#;
    assert!(unbounded_recursive_parent_ctes_in_sql(sql).is_empty());
}

#[test]
fn unrelated_literals_in_one_file_are_not_spliced_into_fake_sql() {
    let source = r##"
        const RECURSION: &str = r#"WITH RECURSIVE numbers(n) AS (
          SELECT 0 UNION ALL SELECT n + 1 FROM numbers WHERE n < 3
        ) SELECT n FROM numbers"#;
        const WAVE_READ: &str = "SELECT parent_wave_id FROM waves";
    "##;
    assert!(unbounded_recursive_parent_ctes(source).is_empty());
}

#[test]
fn literal_concat_is_one_real_sql_candidate_not_two_unrelated_literals() {
    let source = r##"
        const SQL: &str = concat!(
          "WITH RECURSIVE down(id) AS (SELECT id FROM waves UNION ALL ",
          "SELECT w.id FROM waves w JOIN down ON w.parent_wave_id=down.id) SELECT id FROM down"
        );
    "##;
    assert!(!unbounded_recursive_parent_ctes(source).is_empty());
}
