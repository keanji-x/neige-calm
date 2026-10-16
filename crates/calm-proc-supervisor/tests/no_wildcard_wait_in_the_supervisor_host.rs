//! Source lint for consuming non-specific child waits; runtime contracts prove ownership.
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::path::{Path, PathBuf};

const CONSUMING_LITERALS: &[&str] = &[
    "waitpid(-1",
    "libc::wait(",
    "wait4(-1",
    "SYS_wait4",
    "SYS_waitid",
    "Pid::from_raw(-1)",
];
const AUTOREAP_LITERALS: &[&str] = &["SIG_IGN", "SA_NOCLDWAIT"];

#[test]
fn no_wildcard_wait_in_the_supervisor_host() {
    let root = workspace_root();
    let files = rust_sources_under_crates(&root);
    assert!(files.len() > 50, "source gate did not scan the workspace");
    let mut hits = Vec::new();
    for file in files {
        let relative = file.strip_prefix(&root).unwrap().display().to_string();
        let text = std::fs::read_to_string(&file).unwrap();
        let tokens: TokenStream = text
            .parse()
            .unwrap_or_else(|error| panic!("tokenize {relative}: {error}"));
        let mut rendered = String::new();
        render_code(tokens.clone(), &mut rendered);
        for literal in CONSUMING_LITERALS {
            if rendered.contains(literal) {
                hits.push(format!("{relative}: consuming wildcard {literal}"));
            }
        }
        if relative.starts_with("crates/calm-proc-supervisor/src/") {
            for literal in AUTOREAP_LITERALS {
                if rendered.contains(literal) {
                    hits.push(format!("{relative}: auto-reap {literal}"));
                }
            }
        }
        audit_waitid(tokens, false, &relative, &mut hits);
    }
    assert!(
        hits.is_empty(),
        "non-specific consuming child wait or auto-reap found:\n{}",
        hits.join("\n")
    );
}

/// Traverse token groups, including macro bodies. Literals and comments are never executable calls.
fn audit_waitid(stream: TokenStream, permitted_all: bool, file: &str, hits: &mut Vec<String>) {
    let tokens = stream.into_iter().collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], TokenTree::Ident(name) if name == "waitid") {
            if let Some(TokenTree::Group(arguments)) = tokens.get(index + 1)
                && arguments.delimiter() == Delimiter::Parenthesis
            {
                let args = call_arguments(arguments.stream());
                let selector = args.first().map(compact).unwrap_or_default();
                let concrete = matches!(selector.as_str(), "libc::P_PID" | "libc::P_PIDFD");
                let nonconsuming = args.get(3).is_some_and(guarantees_wnowait);
                if !concrete && !nonconsuming {
                    hits.push(format!(
                        "{file}: non-specific consuming waitid({arguments})"
                    ));
                }
                audit_waitid(arguments.stream(), nonconsuming, file, hits);
                index += 2;
                continue;
            }
            // An import, alias, cast or function value would bypass direct-call argument proof.
            hits.push(format!(
                "{file}: waitid must remain a directly audited call"
            ));
        }
        match &tokens[index] {
            TokenTree::Ident(name) if name == "P_ALL" && !permitted_all => hits.push(format!(
                "{file}: P_ALL outside a proven non-consuming waitid"
            )),
            TokenTree::Group(group) => audit_waitid(group.stream(), permitted_all, file, hits),
            _ => {}
        }
        index += 1;
    }
}

#[test]
fn no_wildcard_wait_token_contract() {
    for (source, refused) in [
        ("libc::waitid(libc::P_ALL, 0, info, libc::WEXITED)", true),
        ("libc::waitid(0, 0, info, libc::WEXITED)", true),
        ("libc::waitid(selector, 0, info, flags)", true),
        ("use libc::waitid as collect_children;", true),
        ("let collect_children = libc::waitid;", true),
        ("(libc::waitid)(0, 0, info, flags)", true),
        (
            "libc::waitid(libc::P_ALL, 0, info, libc::WNOWAIT & mask)",
            true,
        ),
        (
            "macro_rules! collect { () => { libc::waitid(0, 0, info, flags) } }",
            true,
        ),
        (
            "libc::waitid(libc::P_ALL, 0, info, libc::WEXITED | libc::WNOWAIT)",
            false,
        ),
        ("libc::waitid(libc::P_PID, pid, info, flags)", false),
        ("let description = \"waitid(0, 0, info, flags)\";", false),
    ] {
        let mut hits = Vec::new();
        audit_waitid(source.parse().unwrap(), false, "token fixture", &mut hits);
        assert_eq!(!hits.is_empty(), refused, "{source}: {hits:?}");
    }
}

fn call_arguments(stream: TokenStream) -> Vec<TokenStream> {
    let mut arguments = vec![TokenStream::new()];
    for token in stream {
        if matches!(&token, TokenTree::Punct(punctuation) if punctuation.as_char() == ',') {
            arguments.push(TokenStream::new());
        } else {
            arguments.last_mut().unwrap().extend([token]);
        }
    }
    arguments
}

fn compact(stream: &TokenStream) -> String {
    stream.to_string().split_whitespace().collect()
}

/// A mandatory OR operand cannot have WNOWAIT cleared by another operand or parenthesized mask.
fn guarantees_wnowait(stream: &TokenStream) -> bool {
    let tokens = stream.clone().into_iter().collect::<Vec<_>>();
    if tokens.len() == 1
        && let TokenTree::Group(group) = &tokens[0]
    {
        return group.delimiter() == Delimiter::Parenthesis && guarantees_wnowait(&group.stream());
    }
    let mut term = TokenStream::new();
    for token in tokens {
        if matches!(&token, TokenTree::Punct(punctuation) if punctuation.as_char() == '|') {
            if compact(&term) == "libc::WNOWAIT" {
                return true;
            }
            term = TokenStream::new();
        } else {
            term.extend([token]);
        }
    }
    compact(&term) == "libc::WNOWAIT"
}

fn render_code(stream: TokenStream, output: &mut String) {
    for token in stream {
        match token {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                output.push_str(open);
                render_code(group.stream(), output);
                output.push_str(close);
            }
            TokenTree::Literal(literal) => {
                let value = literal.to_string();
                if value.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                    output.push_str(&value);
                }
            }
            token => output.push_str(&token.to_string()),
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root is two levels above crates/calm-proc-supervisor")
        .to_path_buf()
}

fn rust_sources_under_crates(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates = root.join("crates");
    let entries =
        std::fs::read_dir(&crates).unwrap_or_else(|e| panic!("read_dir {}: {e}", crates.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        if !entry.path().is_dir() {
            continue;
        }
        for sub in ["src", "tests"] {
            collect_rs(&entry.path().join(sub), &mut out);
        }
    }
    out.sort();
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
