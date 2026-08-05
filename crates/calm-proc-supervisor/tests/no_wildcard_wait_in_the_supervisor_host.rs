//! #1013 T7 — **a lint, not an enforcement.**
//!
//! Grade it honestly: a wildcard wait can walk around any spelling scan —
//! `libc::syscall(libc::SYS_wait4, -1, ..)` assembled at runtime,
//! `nix::sys::wait::waitpid` behind a re-export, or a wrapper in another crate.
//! The *enforced* part of #1013 lives in `pgid_lease` (which target you may
//! compute, and that you may not read the number); the wait side has only this
//! scan plus code review.
//!
//! **Why the rule exists.** Whoever hosts a `ProcRegistry` owns specific child
//! pids and reaps them one by one. A wildcard wait in the same process steals
//! an arbitrary child's exit status out from under that owner — under #1013
//! (PR-B) that means stealing the leader zombie that pins a pid, which is
//! exactly the state the design makes unreachable. The repo already carries the
//! prose form of this rule in
//! `crates/calm-server/tests/cases/forge_merge_crash_reboot.rs`'s
//! `become_subreaper` doc; this test is the version CI can turn red.
//!
//! **Scope is process-level, not crate-level** (D4): the danger belongs to any
//! binary that hosts a registry, and in tests that is `calm-server`'s test
//! binary via `InProcessProcSupervisor`. So the scan covers `crates/*/src` and
//! `crates/*/tests`, not just this crate.
//!
//! **The subreaper combination that this rule makes sharp** (E7): in a process
//! with `PR_SET_CHILD_SUBREAPER`, a supervised proc's *grandchildren* re-parent
//! to the host the moment their leader exits — not when the leader is reaped.
//! The host must then reap them itself, and this rule forbids doing that with
//! `waitpid(-1, ..)`. The two disciplines together mean: **a subreaper host
//! must reap by pid, or accept a pile of unreaped zombies** — and that pile
//! eats `RLIMIT_NPROC`. If you are about to put `become_subreaper` and
//! `InProcessProcSupervisor` in the same binary, that is the tradeoff you are
//! choosing.
//!
//! **The rule is a literal set**, each entry matching only "wildcard":
//! `waitpid(-1`, `libc::wait(`, `wait4(-1`, `SYS_wait4`, `P_ALL`,
//! `Pid::from_raw(-1)`. It deliberately does **not** match the by-pid
//! `fn waitpid(pid: u32)` in `calm-proc-supervisor/src/lib.rs`, which is the
//! blocking wait step for pipe procs and is kept on purpose; a stronger rule
//! ("no bare `waitpid(` outside `pgid_lease`/`Drop`") would be red on arrival.
//! It equally does not catch a diagnostic `waitpid(entry.pid, WNOHANG)` added
//! somewhere new — that one is code review's job.
//!
//! Two `SIGCHLD` literals ride along on the same scan, restricted to
//! `crates/calm-proc-supervisor/src`: setting `SIGCHLD` to `SIG_IGN` or setting
//! `SA_NOCLDWAIT` makes the kernel auto-reap, which destroys the ownership
//! invariant PR-B's pin depends on.

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

/// Literals that make the kernel auto-reap our children. Scanned only under
/// `crates/calm-proc-supervisor/src` (§2.4(a)).
const AUTOREAP_LITERALS: &[&str] = &["SIG_IGN", "SA_NOCLDWAIT"];

const AUTOREAP_SCOPE: &str = "crates/calm-proc-supervisor/src";

/// This file spells every literal above, so it would match itself. Excluded by
/// path — comment-stripping alone is not enough (D4/C9: the previous shape of
/// this rule handled comment self-reference but not *file* self-reference).
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

/// Strips a `//` line comment so the repo's existing written-down version of
/// this discipline (which quotes `waitpid(-1, ..)` verbatim) does not trip the
/// scan. The `://` guard keeps URLs in string literals from truncating a line
/// early.
///
/// **Admitted bypass, and it is the UNSAFE direction.** This scanner is not
/// token-aware: a `//` inside any string literal other than a `://` URL
/// truncates the line, so a forbidden call after it is never seen. Concretely,
/// this line passes the lint:
///
/// ```text
/// let m = "not a URL //"; unsafe { libc::wait(std::ptr::null_mut()); }
/// ```
///
/// For a *prohibition* lint, missing a violation is the direction that hurts —
/// calling it "the safe direction" (as this comment previously did) inverts the
/// risk. What is actually true is narrower: the lint is aimed at the accidental
/// case, and the accidental case does not hide behind a `//` in a string
/// literal. It provides no resistance at all to a deliberate bypass.
///
/// Follow-up if someone judges it worth the code: make the scanner
/// string-literal-aware (track `"` / `r#"` state) instead of stripping on the
/// first non-`:`-prefixed `//`. Deliberately out of scope for #1013 PR-A.
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
