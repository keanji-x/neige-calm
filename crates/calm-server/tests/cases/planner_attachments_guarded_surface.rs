//! Class-level audit for `planner_attachments`: no file in it except `dir.rs` may NAME a path-resolving
//! filesystem function. It matches names only; an alias, re-export or raw `libc` would not be matched.

use std::path::{Path, PathBuf};

/// The module under audit.
fn module_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/planner_attachments")
}

/// The one file allowed to perform filesystem operations: everything it does is a descriptor plus a single name.
const GUARDED: &str = "dir.rs";

/// Test code plants the symlink an attacker would plant, so it is out of scope.
const TEST_FILE: &str = "tests.rs";

/// Every spelling that resolves a caller-supplied path: `std::fs` free functions plus the path-taking `File`/`OpenOptions`
/// constructors (`tokio::fs` mirrors the names). `File` itself and `PathBuf::join` are deliberately absent: they resolve nothing.
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
    // `fs::read(` and not the bare `fs::read`: `read_file_raw_response_from_handle` takes an already-open descriptor and must stay callable.
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
            // A line that only talks about the hazard is documentation; this module's docs name these functions on purpose.
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

/// Without this, moving a path-based call into `dir.rs` would silence the audit while changing nothing.
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
    // The positive half: it really holds the descriptor operations, so the exemption is buying something.
    for required in ["openat", "renameat", "unlinkat", "mkdirat", "fstatat"] {
        assert!(
            source.contains(required),
            "`{GUARDED}` is exempt because it uses `{required}`; it does not"
        );
    }
}
