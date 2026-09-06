//! The class-level audit for `planner_attachments`: no file in it except
//! `dir.rs` may NAME a path-resolving filesystem function.
//!
//! # Why this is a grep and not a review checklist
//!
//! Three review rounds fixed call sites. Each fix was right and each was an
//! enumeration — *these* sites now resolve safely — and the next round found
//! the next site in the same class: `resolve()`, then the read open, then the
//! bind's destination, then the sweep's `read_dir`/`remove_file` and the whole
//! upload write path. The defect was never a call site; it was that
//! `std::fs::read_dir`, `File::create`, `fs::rename`, `create_dir_all` and
//! `remove_file` on a JOINED PATH were expressible in a module whose threat
//! model is an agent that can write the workspace.
//!
//! So the question this file asks is not "did we get every call site this
//! time" — which has been answered wrong three times — but "does this module
//! name any of those functions at all", which is decidable, cheap, and stays
//! true as the module grows.
//!
//! # What it does not establish
//!
//! It matches NAMES. Code reaching the same syscalls through an alias, a
//! re-export under a different name, or raw `libc` would not be matched. The
//! banned list therefore covers the module prefixes and the `use` forms that
//! bring them in unqualified, which closes the ordinary ways of writing it; a
//! deliberate rename is outside its reach and it does not claim otherwise.
//! What makes the safe path the easy one is the type discipline in
//! `planner_attachments::dir` — no path-taking entry point, no accessor that
//! yields a `Path` or a `RawFd` — and this file is the backstop for that, not
//! a substitute.

use std::path::{Path, PathBuf};

/// The module under audit.
fn module_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/planner_attachments")
}

/// The one file allowed to perform filesystem operations, and the reason it is
/// allowed: everything it does is a descriptor plus a single name.
const GUARDED: &str = "dir.rs";

/// Test code builds fixtures on disk — planting the symlink an attacker would
/// plant is the point — so it is out of scope. Production code is what the
/// audit is about.
const TEST_FILE: &str = "tests.rs";

/// Every spelling that resolves a caller-supplied path.
///
/// This is `std::fs`'s free-function surface plus the path-taking `File` and
/// `OpenOptions` constructors, which is the whole of the standard library's
/// path-based filesystem API; `tokio::fs` mirrors the same names, so matching
/// the bare identifiers covers both. It deliberately does NOT list
/// `std::fs::File` or `tokio::fs::File` themselves: those are the TYPE, and
/// `File::from_raw_fd` / `File::from_std` take a descriptor somebody else
/// already resolved — which is exactly what `dir` hands out.
///
/// `PathBuf::join` is not here either. Joining builds a string; it resolves
/// nothing. `bound_file_path` builds the absolute path a queue entry records
/// for codex to open in its own process, and that is a name handed onward, not
/// a call this module makes.
const BANNED: &[&str] = &[
    "read_dir",
    "read_link",
    "read_to_string",
    "create_dir",
    "remove_file",
    "remove_dir",
    "symlink_metadata",
    "canonicalize",
    "hard_link",
    "soft_link",
    "set_permissions",
    "try_exists",
    "File::open",
    "File::create",
    "OpenOptions",
    "fs::rename",
    "fs::copy",
    "fs::write",
    // `fs::read(` and not the bare `fs::read`: `routes::fs::
    // read_file_raw_response_from_handle` takes an ALREADY-OPEN descriptor and
    // is the vetted response builder this module is supposed to call. A needle
    // that caught it would be a needle the next reader learns to work around.
    "fs::read(",
    "fs::metadata",
    "fs::exists",
    "fs::symlink",
    "fs::remove_",
    "fs::create_",
];

#[test]
fn no_file_outside_the_guarded_one_names_a_path_resolving_filesystem_call() {
    let dir = module_dir();
    let mut offences = Vec::new();
    let mut audited = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the module directory is readable") {
        let entry = entry.expect("a directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == GUARDED || name == TEST_FILE {
            continue;
        }
        audited.push(name.clone());
        let source = std::fs::read_to_string(entry.path()).expect("a source file is readable");
        for (number, line) in source.lines().enumerate() {
            // A line that only TALKS about the hazard is documentation, and
            // this module's docs name these functions constantly — on purpose,
            // because saying which operations are forbidden is how the next
            // reader learns the rule.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("/*") || code.starts_with('*') {
                continue;
            }
            for needle in BANNED {
                if line.contains(needle) {
                    offences.push(format!("{name}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        audited.len() >= 4,
        "the audit must actually have read the module; it saw {audited:?}"
    );
    assert!(
        audited.iter().any(|name| name == "store.rs")
            && audited.iter().any(|name| name == "gc.rs")
            && audited.iter().any(|name| name == "bind.rs")
            && audited.iter().any(|name| name == "mod.rs"),
        "the four files that have each shipped one of these defects must be in \
         scope; saw {audited:?}"
    );
    assert!(
        offences.is_empty(),
        "planner_attachments must reach the filesystem only through `dir`, which resolves a \
         descriptor plus one name. These lines name a path-resolving call:\n{}",
        offences.join("\n")
    );
}

/// The guarded file has to actually BE guarded: it is exempt from the list
/// above, so nothing else may be.
///
/// Without this, moving a path-based call into `dir.rs` would silence the
/// audit while changing nothing — the exemption is for the file that resolves
/// through `openat2` and descriptors, not for any file that happens to be
/// named `dir.rs`.
#[test]
fn the_guarded_file_reaches_the_filesystem_only_through_descriptors() {
    let source =
        std::fs::read_to_string(module_dir().join(GUARDED)).expect("the guarded file is readable");
    for spelling in BANNED {
        let named = source
            .lines()
            .filter(|line| {
                let code = line.trim_start();
                !code.starts_with("//") && !code.starts_with('*') && line.contains(spelling)
            })
            .collect::<Vec<_>>();
        assert!(
            named.is_empty(),
            "`{GUARDED}` performs no path-based filesystem call, but names `{spelling}`:\n{}",
            named.join("\n")
        );
    }
    // And the positive half: it really is the file that holds the descriptor
    // operations, so the exemption is buying something.
    for required in ["openat", "renameat", "unlinkat", "mkdirat", "fstatat"] {
        assert!(
            source.contains(required),
            "`{GUARDED}` is exempt because it uses `{required}`; it does not"
        );
    }
}
