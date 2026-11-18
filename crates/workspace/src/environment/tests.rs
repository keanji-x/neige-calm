use super::*;
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

struct Fixture {
    _root: tempfile::TempDir,
    project: std::path::PathBuf,
    base: BTreeMap<OsString, OsString>,
}

impl Fixture {
    fn new(rc: &str) -> Self {
        fs::create_dir_all("_local/tmp").unwrap();
        let root = tempfile::tempdir_in("_local/tmp").unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let project = root_path.join("project");
        let home = root_path.join("home");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&home).unwrap();
        // Fence discovery before any developer's ancestor .envrc can be considered.
        fs::write(root_path.join(".envrc"), "# isolated fixture root\n").unwrap();
        fs::write(project.join(".envrc"), rc).unwrap();
        let base = BTreeMap::from([
            ("HOME".into(), home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join("config").into_os_string(),
            ),
            ("XDG_DATA_HOME".into(), home.join("data").into_os_string()),
            (
                "DIRENV_CONFIG".into(),
                home.join("config/direnv").into_os_string(),
            ),
        ]);
        Self {
            _root: root,
            project,
            base,
        }
    }

    fn allow(&self, directory: &Path) {
        let output = crate::bounded_git::output(
            Command::new("direnv")
                .arg("allow")
                .arg(directory)
                .env_clear()
                .envs(&self.base),
            Instant::now() + Duration::from_secs(10),
        )
        .expect("real direnv must be installed");
        assert!(output.status.success());
    }

    fn resolve(&self) -> Environment {
        resolve(&self.project, self.base.clone()).unwrap()
    }
}

#[test]
fn directory_environment_loads_parent_direnv_and_preserves_base() {
    let fixture = Fixture::new("export PROJECT_ID=parent-project\n");
    fixture.allow(&fixture.project);
    let child = fixture.project.join("nested");
    fs::create_dir(&child).unwrap();
    let env = resolve(&child, fixture.base.clone()).unwrap();
    assert_eq!(
        env.get(OsStr::new("PROJECT_ID")),
        Some(OsStr::new("parent-project"))
    );
    assert_eq!(
        env.get(OsStr::new("HOME")),
        fixture
            .base
            .get(OsStr::new("HOME"))
            .map(OsString::as_os_str)
    );
}

#[test]
fn directory_environment_uses_nearest_rc() {
    let fixture = Fixture::new("export PROJECT_ID=outer\n");
    fixture.allow(&fixture.project);
    let child = fixture.project.join("nested");
    fs::create_dir(&child).unwrap();
    fs::write(child.join(".envrc"), "export PROJECT_ID=inner\n").unwrap();
    fixture.allow(&child);
    let env = resolve(&child, fixture.base.clone()).unwrap();
    assert_eq!(env.get(OsStr::new("PROJECT_ID")), Some(OsStr::new("inner")));
}

#[test]
fn directory_environment_keeps_separate_project_values_concurrently() {
    let first = Fixture::new("export PROJECT_SECRET=synthetic-first\n");
    let second = Fixture::new("export PROJECT_SECRET=synthetic-second\n");
    first.allow(&first.project);
    second.allow(&second.project);
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| first.resolve());
        let two = scope.spawn(|| second.resolve());
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(
        one.get(OsStr::new("PROJECT_SECRET")),
        Some(OsStr::new("synthetic-first"))
    );
    assert_eq!(
        two.get(OsStr::new("PROJECT_SECRET")),
        Some(OsStr::new("synthetic-second"))
    );
}

#[test]
fn directory_environment_applies_removal_empty_and_multiline_values() {
    let mut fixture = Fixture::new(
        "unset OLD_PROJECT_SECRET\nexport EMPTY_VALUE=''\nexport MULTILINE_VALUE=$'first\\nsecond\\n'\n",
    );
    fixture
        .base
        .insert("OLD_PROJECT_SECRET".into(), "synthetic-old".into());
    fixture.allow(&fixture.project);
    let values = fixture.resolve().into_values();
    assert!(!values.contains_key(OsStr::new("OLD_PROJECT_SECRET")));
    assert_eq!(
        values.get(OsStr::new("EMPTY_VALUE")),
        Some(&OsString::from(""))
    );
    assert_eq!(
        values.get(OsStr::new("MULTILINE_VALUE")),
        Some(&OsString::from("first\nsecond\n"))
    );
}

#[test]
fn directory_environment_refuses_unapproved_rc() {
    let fixture = Fixture::new("export PROJECT_SECRET=synthetic-unapproved\n");
    let error = resolve(&fixture.project, fixture.base.clone())
        .err()
        .expect("unapproved rc must fail");
    assert!(matches!(error, EnvironmentError::Failed(_)));
    assert!(!format!("{error:?} {error}").contains("synthetic-unapproved"));
}

#[test]
fn directory_environment_refuses_rc_failure_without_exposing_output() {
    let fixture = Fixture::new(
        "export PROJECT_SECRET=synthetic-failed\nprintf 'synthetic-failed' >&2\nexit 17\n",
    );
    fixture.allow(&fixture.project);
    let error = resolve(&fixture.project, fixture.base.clone())
        .err()
        .expect("failed rc must fail");
    assert!(matches!(error, EnvironmentError::Failed(_)));
    assert!(!format!("{error:?} {error}").contains("synthetic-failed"));
}

#[test]
fn directory_environment_reports_missing_direnv() {
    let mut fixture = Fixture::new("export PROJECT_ID=project\n");
    fixture.base.insert(
        "PATH".into(),
        fixture.project.join("missing-bin").into_os_string(),
    );
    let error = resolve(&fixture.project, fixture.base)
        .err()
        .expect("missing executable must fail");
    assert_eq!(error, EnvironmentError::MissingDirenv);
}

#[test]
fn directory_environment_preserves_missing_source_error() {
    let fixture = Fixture::new("# fixture\n");
    let error = resolve(&fixture.project.join("missing"), fixture.base)
        .err()
        .expect("missing directory must fail");
    assert_eq!(
        error,
        EnvironmentError::SourceUnavailable(io::ErrorKind::NotFound)
    );
}

#[test]
fn directory_environment_preserves_non_directory_source_error() {
    let fixture = Fixture::new("# fixture\n");
    let error = resolve(&fixture.project.join(".envrc"), fixture.base)
        .err()
        .expect("file source must fail");
    assert_eq!(
        error,
        EnvironmentError::SourceUnavailable(io::ErrorKind::NotADirectory)
    );
}

#[test]
fn directory_environment_accepts_real_direnv_unchanged_empty_output() {
    let fixture = Fixture::new("export PROJECT_ID=stable\n");
    fixture.allow(&fixture.project);
    let base = fixture.resolve().into_values();
    let output = crate::bounded_git::output(
        Command::new("direnv")
            .args(["export", "json"])
            .current_dir(&fixture.project)
            .env_clear()
            .envs(&base),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "up-to-date real direnv emits no delta"
    );
    let env = resolve(&fixture.project, base).unwrap();
    assert_eq!(
        env.get(OsStr::new("PROJECT_ID")),
        Some(OsStr::new("stable"))
    );
}

#[test]
fn directory_environment_without_rc_keeps_base_without_direnv() {
    let mut fixture = Fixture::new("# fixture\n");
    // A filesystem root has no developer home ancestors. Assert the premise so
    // a machine deliberately configured with a root .envrc never loads it here.
    let root = fixture.project.ancestors().last().unwrap();
    assert!(
        matches!(fs::symlink_metadata(root.join(".envrc")), Err(error) if error.kind() == io::ErrorKind::NotFound)
    );
    fixture.base.insert(
        "PATH".into(),
        fixture.project.join("missing-bin").into_os_string(),
    );
    let expected = fixture.base.clone();
    assert_eq!(resolve(root, fixture.base).unwrap().into_values(), expected);
}

#[test]
fn directory_environment_rejects_malformed_external_output_without_exposing_it() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new("# fixture\n");
    let bin = fixture.project.join("bin");
    fs::create_dir(&bin).unwrap();
    let command = bin.join("direnv");
    // A deliberately broken dependency exercises the JSON boundary; normal
    // directory loading above uses the real direnv executable.
    fs::write(
        &command,
        "#!/bin/sh\nprintf '%s' '{\"SECRET\":\"synthetic-malformed\",\"INVALID\":false}'\n",
    )
    .unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.base.insert("PATH".into(), bin.into_os_string());
    let error = resolve(&fixture.project, fixture.base)
        .err()
        .expect("invalid dependency output must fail");
    assert_eq!(error, EnvironmentError::InvalidOutput);
    assert!(!format!("{error:?} {error}").contains("synthetic-malformed"));
}

#[test]
fn directory_environment_bounds_slow_real_direnv() {
    let fixture = Fixture::new("sleep 15\nexport PROJECT_ID=too-late\n");
    fixture.allow(&fixture.project);
    let started = Instant::now();
    let error = resolve(&fixture.project, fixture.base.clone())
        .err()
        .expect("slow rc must fail");
    assert_eq!(error, EnvironmentError::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(14));
}
