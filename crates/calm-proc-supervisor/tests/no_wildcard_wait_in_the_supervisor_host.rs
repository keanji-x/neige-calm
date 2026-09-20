//! A lint, not an enforcement: a text scan for wildcard child waits in any binary that hosts a `ProcRegistry`, plus the two auto-reap `SIGCHLD` literals under this crate's src.
//! A wildcard wait steals the leader zombie that pins a pid; a subreaper host must therefore reap by pid.

use std::path::{Path, PathBuf};

/// Literals that mean "wait for *any* child". Scanned over `crates/*/src` and
/// `crates/*/tests`.
const WILDCARD_WAIT_LITERALS: &[&str] = &[
    "waitpid(-1",
    "libc::wait(",
    "wait4(-1",
    "SYS_wait4",
    "P_ALL",
    "Pid::from_raw(-1)",
];

/// Literals that make the kernel auto-reap our children. Scanned only under `crates/calm-proc-supervisor/src`.
const AUTOREAP_LITERALS: &[&str] = &["SIG_IGN", "SA_NOCLDWAIT"];

const AUTOREAP_SCOPE: &str = "crates/calm-proc-supervisor/src";

/// This file spells every literal above, so it would match itself; excluded by path.
const SELF: &str = "crates/calm-proc-supervisor/tests/no_wildcard_wait_in_the_supervisor_host.rs";

#[test]
fn no_wildcard_wait_in_the_supervisor_host() {
    let root = workspace_root();
    let files = rust_sources_under_crates(&root);
    assert!(
        files.len() > 50,
        "scanner found only {} files under {}; it is not actually scanning the workspace",
        files.len(),
        root.display()
    );

    let mut hits: Vec<String> = Vec::new();
    let mut scanned_self = false;
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        if rel == SELF {
            scanned_self = true;
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| {
            panic!("read {}: {e}", file.display());
        });
        let in_autoreap_scope = rel.starts_with(AUTOREAP_SCOPE);
        for (idx, line) in text.lines().enumerate() {
            let code = strip_line_comment(line);
            for literal in WILDCARD_WAIT_LITERALS {
                if code.contains(literal) {
                    hits.push(format!("{rel}:{}: {literal} — {}", idx + 1, code.trim()));
                }
            }
            if in_autoreap_scope {
                for literal in AUTOREAP_LITERALS {
                    if code.contains(literal) {
                        hits.push(format!("{rel}:{}: {literal} — {}", idx + 1, code.trim()));
                    }
                }
            }
        }
    }

    assert!(
        scanned_self,
        "the self-exclusion path {SELF} did not match any scanned file; the \
         scanner has moved or been renamed and is now matching itself (or the \
         exclusion is silently disabled)"
    );
    assert!(
        hits.is_empty(),
        "wildcard wait / auto-reap literal(s) found in a process that may host a \
         ProcRegistry. Whoever hosts the registry owns specific pids and must \
         reap them by pid (#1013). If a host legitimately needs to collect \
         re-parented grandchildren, collect them by pid.\n{}",
        hits.join("\n")
    );
}

/// Strips a `//` line comment (the `://` guard keeps URLs from truncating a line early).
/// Not token-aware: a `//` inside a string literal truncates the line and hides a call after it — no resistance to a deliberate bypass.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' && (i == 0 || bytes[i - 1] != b':') {
            return &line[..i];
        }
        i += 1;
    }
    line
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
