//! The `--templates-dir` loader, on hand-built directories. Every refusal case asserts `Err`
//! and that the error names the offending file.

use super::*;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use calm_types::report_blocks::{KIND_TASK, render_fence};
use calm_types::report_contract::canonical_line;
use calm_types::track_report::work_brief_header;
use clap::Parser;
use serde_json::json;
use tempfile::TempDir;

/// A body every check accepts: canonical work-brief header on line 1, a
/// closed contract comment, prose, one canonical `task` fence.
fn valid_body() -> String {
    let mut body = canonical_line(&work_brief_header());
    body.push_str("\n<!-- site template: closed contract comment -->\n\n# Plan\n\n");
    body.push_str("Operator prose.\n\n");
    body.push_str(&render_fence(
        KIND_TASK,
        &json!({
            "key": "site-task",
            "kind": "codex",
            "goal": "Do the operator's thing.",
            "acceptance": "It is done.",
            "declared_by": PLANNER_DECLARATION_AUTHOR,
            "depends_on": [],
            "no_gate_reason": "site fixture",
            "ready": false
        }),
    ));
    body
}

fn file(id: &str, title: &str, body: &str) -> String {
    format!("+++\nid = \"{id}\"\ntitle = \"{title}\"\n+++\n{body}")
}

/// Write `(file name, contents)` pairs into a fresh directory.
fn site_dir(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).expect("write template file");
    }
    dir
}

/// The production entry point on one directory, exactly what `AppState::boot` calls.
fn load(dir: &Path) -> Result<&'static TemplateRoster, RosterError> {
    TemplateRoster::for_boot(Some(dir))
}

/// `Err`, and its message names `path`.
#[track_caller]
fn assert_refused_naming<T>(result: Result<T, RosterError>, path: &Path) -> String {
    let error = match result {
        Ok(_) => panic!("expected a refusal naming {}, got Ok", path.display()),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains(&path.display().to_string()),
        "the error must name {}; got: {message}",
        path.display()
    );
    message
}

#[test]
fn site_files_become_site_entries_after_the_builtins_in_file_name_order() {
    let body = valid_body();
    let dir = site_dir(&[
        ("b.md", &file("b", "Site B", &body)),
        ("a.md", &file("a", "Site A", &body)),
        ("notes.txt", "not a template; not `.md`, so not read"),
    ]);
    let roster = load(dir.path()).expect("valid site dir");
    let builtin = TemplateRoster::builtin();
    let keys: Vec<&str> = roster.entries().iter().map(Template::key).collect();
    let mut expected: Vec<&str> = builtin.entries().iter().map(Template::key).collect();
    expected.extend(["site/a", "site/b"]);
    assert_eq!(
        keys, expected,
        "builtin first, then site files sorted by name"
    );

    for (key, title) in [("site/a", "Site A"), ("site/b", "Site B")] {
        let template = roster.get(key).expect("site entry admits");
        assert_eq!(template.title(), title);
        let recipe = template.recipe();
        assert_eq!(recipe.summary, title);
        assert_eq!(
            recipe.body, body,
            "{key}: the recipe body is the file after the front matter"
        );
        assert!(
            std::ptr::eq(template.key().as_ptr(), template.key.as_ptr()),
            "{key}: key() is the entry's own buffer"
        );
    }
    // The unprefixed spelling is not a key: a request for `a` is a 400.
    assert!(roster.get("a").is_none());
    assert!(roster.get("b").is_none());

    // The builtin half is the builtin roster's own entries — same buffers, nothing re-leaked.
    for (merged, original) in roster.entries().iter().zip(builtin.entries()) {
        assert!(std::ptr::eq(merged.key.as_ptr(), original.key.as_ptr()));
        assert!(std::ptr::eq(merged.title.as_ptr(), original.title.as_ptr()));
        assert!(std::ptr::eq(merged.body.as_ptr(), original.body.as_ptr()));
    }
}

#[test]
fn for_boot_without_a_dir_is_the_builtin_roster_itself() {
    let roster = TemplateRoster::for_boot(None).expect("no dir is fine");
    assert!(std::ptr::eq(roster, TemplateRoster::builtin()));
}

#[test]
fn for_boot_with_a_dir_serves_builtin_and_site_entries() {
    let body = valid_body();
    let dir = site_dir(&[("x.md", &file("x", "Site X", &body))]);
    let roster = TemplateRoster::for_boot(Some(dir.path())).expect("valid site dir");
    assert!(!std::ptr::eq(roster, TemplateRoster::builtin()));
    assert_eq!(
        roster.entries().len(),
        TemplateRoster::builtin().entries().len() + 1
    );
    assert!(roster.get(DEV).is_some());
    assert_eq!(roster.get("site/x").expect("site/x").title(), "Site X");
}

#[test]
fn a_config_with_a_bad_templates_dir_fails_for_boot() {
    let dir = site_dir(&[("x.md", "# no front matter\n")]);
    let cfg = crate::config::Config::parse_from([
        "calm-server",
        "--templates-dir",
        dir.path().to_str().expect("utf-8 tempdir"),
    ]);
    assert_eq!(cfg.templates_dir.as_deref(), Some(dir.path()));
    let error = TemplateRoster::for_boot(cfg.templates_dir.as_deref())
        .err()
        .expect("a bad templates dir must fail the boot");
    assert!(
        error.to_string().contains("x.md"),
        "the boot error must name the file: {error}"
    );
}

#[test]
fn a_dir_with_no_md_files_is_ok_and_adds_nothing() {
    let dir = site_dir(&[("README.txt", "nothing here")]);
    let roster = load(dir.path()).expect("an empty site set is not an error");
    assert_eq!(
        roster.entries().len(),
        TemplateRoster::builtin().entries().len()
    );
}

#[test]
fn a_missing_or_unlistable_dir_is_refused_naming_it() {
    let parent = TempDir::new().expect("tempdir");
    let missing = parent.path().join("does-not-exist");
    let message = assert_refused_naming(load(&missing), &missing);
    assert!(message.starts_with("templates dir "), "{message}");

    // A file where a directory was expected.
    let not_a_dir = parent.path().join("file-not-dir");
    std::fs::write(&not_a_dir, "x").unwrap();
    assert_refused_naming(load(&not_a_dir), &not_a_dir);
}

#[test]
fn a_file_without_front_matter_is_refused_naming_it() {
    let dir = site_dir(&[("x.md", "# no front matter at all\n")]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(
        message.contains("must open with a `+++`"),
        "the front matter parser's own reason: {message}"
    );
}

#[test]
fn a_file_with_unparsable_front_matter_is_refused_naming_it() {
    let dir = site_dir(&[
        ("ok.md", &file("ok", "Fine", &valid_body())),
        ("x.md", "+++\nid = x\ntitle = \"X\"\n+++\n# body\n"),
    ]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(message.contains("front matter"), "{message}");
}

#[test]
fn an_id_that_is_not_the_stem_is_refused_naming_it() {
    let dir = site_dir(&[("x.md", &file("y", "Y", &valid_body()))]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(
        message.contains("id `y` must equal the file stem `x`"),
        "{message}"
    );
    assert!(
        message.contains("site/x"),
        "names the key it would have had: {message}"
    );
}

#[test]
fn two_files_declaring_one_id_are_refused_by_the_stem_rule() {
    let body = valid_body();
    let dir = site_dir(&[
        ("x.md", &file("x", "X", &body)),
        ("y.md", &file("x", "X again", &body)),
    ]);
    let path = dir.path().join("y.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(
        message.contains("must equal the file stem `y`"),
        "{message}"
    );
}

/// The duplicate arm is only reachable with the same stem in two directories.
#[test]
fn one_stem_from_two_directories_is_a_duplicate_naming_the_second() {
    let body = valid_body();
    let first = site_dir(&[("x.md", &file("x", "First", &body))]);
    let second = site_dir(&[("x.md", &file("x", "Second", &body))]);
    let paths = [first.path().join("x.md"), second.path().join("x.md")];
    let mut roster = TemplateRoster::builtin().copy_of_entries();
    let error = roster
        .extend_with_site_files(&paths)
        .expect_err("one key twice");
    let message = error.to_string();
    assert!(
        message.contains(&paths[1].display().to_string()),
        "{message}"
    );
    assert!(
        message.contains("template id `site/x` is already on the roster"),
        "{message}"
    );
    assert!(
        matches!(
            error,
            RosterError::SiteFile {
                reason: SiteFileError::Duplicate(_),
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn an_unreadable_file_is_refused_naming_it() {
    // A directory named `x.md`: the extension filter deliberately does not skip it.
    let dir = site_dir(&[]);
    let as_dir = dir.path().join("x.md");
    std::fs::create_dir(&as_dir).unwrap();
    assert_refused_naming(load(dir.path()), &as_dir);

    // Only meaningful when the test process is not privileged; a root run reads it regardless.
    let dir = site_dir(&[("y.md", &file("y", "Y", &valid_body()))]);
    let unreadable = dir.path().join("y.md");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&unreadable).is_ok() {
        eprintln!("privileged test process: the mode-000 half of this case is skipped");
    } else {
        assert_refused_naming(load(dir.path()), &unreadable);
    }
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_body_with_a_malformed_task_fence_is_refused_naming_it() {
    let mut body = valid_body();
    body.push_str("\n```neige-block task\nnot json\n```\n");
    let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(message.contains("body does not compile"), "{message}");
}

#[test]
fn a_body_with_a_schema_invalid_task_fence_is_refused_naming_it() {
    let mut body = valid_body();
    body.push_str(&render_fence(KIND_TASK, &json!({ "key": "Not A Key" })));
    let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(message.contains("body does not compile"), "{message}");
}

/// A non-canonical or misplaced header passes `compile_template` and would fail every create.
#[test]
fn a_body_failing_the_contract_header_funnel_is_refused_naming_it() {
    let canonical = canonical_line(&work_brief_header());
    let spaced = canonical.replacen("\"version\":1", "\"version\": 1", 1);
    assert_ne!(spaced, canonical, "the fixture must be non-canonical");
    let body = format!("{spaced}\n\n# Plan\n\nprose\n");
    let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(message.contains("report contract header"), "{message}");

    let misplaced = format!("# Plan\n\n{canonical}\n\nprose\n");
    let dir = site_dir(&[("x.md", &file("x", "X", &misplaced))]);
    let path = dir.path().join("x.md");
    let message = assert_refused_naming(load(dir.path()), &path);
    assert!(message.contains("report contract header"), "{message}");
}

#[test]
fn a_stem_outside_the_id_alphabet_is_refused_naming_it() {
    for name in ["Site-X.md", "a_b.md", "x y.md"] {
        let stem = name.strip_suffix(".md").unwrap();
        let dir = site_dir(&[(name, &file(stem, "X", &valid_body()))]);
        let path = dir.path().join(name);
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("must match"), "{name}: {message}");
    }
}

#[test]
fn a_site_file_named_like_a_builtin_is_a_distinct_site_entry() {
    let name = format!("{DEV}.md");
    let dir = site_dir(&[(
        name.as_str(),
        &file(DEV, "Operator's issue development", &valid_body()),
    )]);
    let roster = load(dir.path()).expect("a builtin-named site file is not a collision");
    let builtin = TemplateRoster::builtin().get(DEV).expect("builtin");
    let kept = roster.get(DEV).expect("the builtin key still admits");
    assert!(
        std::ptr::eq(kept.key.as_ptr(), builtin.key.as_ptr()),
        "the builtin entry must be untouched, not overridden"
    );
    assert_eq!(kept.title(), builtin.title());
    let site_key = format!("{SITE_PREFIX}{DEV}");
    let site = roster
        .get(&site_key)
        .expect("the site entry admits under site/");
    assert_eq!(site.title(), "Operator's issue development");
    assert_ne!(site.recipe().body, builtin.recipe().body);
    assert_eq!(
        roster
            .entries()
            .iter()
            .filter(|t| t.key().ends_with(DEV))
            .count(),
        2,
        "one builtin, one site entry"
    );
}
