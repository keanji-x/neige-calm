use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cli(home: &Path, config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_neige-app"))
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", "/usr/bin:/bin")
        .env("USER", "neige-test")
        .args(["system"])
        .args(args)
        .arg("--config")
        .arg(config)
        .output()
        .expect("run real CLI")
}

fn config(root: &Path, label: &str, systemd: &str) -> std::path::PathBuf {
    let path = root.join(format!("{label}.toml"));
    fs::write(
        &path,
        format!(
            "[admin]\ntoken_file = \"{}\"\n[systemd]\n{systemd}\n",
            root.join(format!("{label}.token")).display()
        ),
    )
    .expect("write isolated config");
    path
}

fn success(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("install JSON")
}

#[test]
fn system_install_custom_name_keeps_default_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let first = config(tmp.path(), "first", "scope = \"user\"");
    success(&cli(&home, &first, &["install"]));
    let original_path = home.join(".config/systemd/user/neige-app.service");
    let original = fs::read(&original_path).unwrap();
    for (label, name, filename) in [
        ("second", "neige-second", "neige-second.service"),
        ("third", "neige-third.service", "neige-third.service"),
    ] {
        let cfg = config(tmp.path(), label, &format!("unit_name = \"{name}\""));
        let result = success(&cli(&home, &cfg, &["install"]));
        let target = home.join(".config/systemd/user").join(filename);
        assert_eq!(result["unit"], target.to_str().unwrap());
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains(cfg.to_str().unwrap())
        );
        assert_eq!(
            result["nextSteps"][1],
            format!("systemctl --user enable --now {name}")
        );
        assert_eq!(fs::read(&original_path).unwrap(), original);
    }
}

#[test]
fn system_install_refuses_conflict_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let target = home.join(".config/systemd/user/neige@blue.service");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"original unit\0bytes").unwrap();
    for label in ["missing-token", "existing-token"] {
        let cfg = config(tmp.path(), label, "unit_name = \"neige@blue\"");
        let token = tmp.path().join(format!("{label}.token"));
        if label == "existing-token" {
            fs::write(&token, b"original token\n").unwrap();
        }
        let output = cli(&home, &cfg, &["install"]);
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        for expected in [
            "already exists",
            "systemd.unit_name",
            "systemd.unit_path",
            "neige@blue",
            "User",
            cfg.to_str().unwrap(),
            target.to_str().unwrap(),
        ] {
            assert!(err.contains(expected), "{err}");
        }
        assert!(err.find("independent").unwrap() < err.find("--force").unwrap());
        assert!(err.contains("only to intentionally replace this target"));
        assert_eq!(fs::read(&target).unwrap(), b"original unit\0bytes");
        if label == "existing-token" {
            assert_eq!(fs::read(&token).unwrap(), b"original token\n");
        } else {
            assert!(!token.exists());
        }
    }
}

#[test]
fn system_install_explicit_path_and_force_select_only_target() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let other = tmp.path().join("other.service");
    fs::write(&other, "other unit").unwrap();
    for scope in ["user", "system"] {
        let target = tmp.path().join(format!("{scope}-explicit.service"));
        let cfg = config(
            tmp.path(),
            scope,
            &format!(
                "scope = \"{scope}\"\nunit_name = \"different.service\"\nunit_path = \"{}\"\nuser = \"neige-test\"\nhome = \"{}\"",
                target.display(),
                home.display()
            ),
        );
        let token = tmp.path().join(format!("{scope}.token"));
        fs::write(&token, "keep token").unwrap();
        let result = success(&cli(&home, &cfg, &["install"]));
        assert_eq!(result["unit"], target.to_str().unwrap());
        let expected = if scope == "user" {
            "systemctl --user enable --now different.service"
        } else {
            "systemctl enable --now different.service"
        };
        assert_eq!(result["nextSteps"][1], expected);
        fs::write(&target, "replace selected").unwrap();
        success(&cli(&home, &cfg, &["install", "--force"]));
        let unit = fs::read_to_string(&target).unwrap();
        assert!(unit.contains("Description=different.service"));
        assert!(unit.contains(cfg.to_str().unwrap()));
        assert_eq!(unit.contains("User=neige-test"), scope == "system");
        assert_eq!(fs::read(&token).unwrap(), b"keep token");
        assert_eq!(fs::read(&other).unwrap(), b"other unit");
        assert!(!home.join(".config/systemd/user/different.service").exists());
    }
}

#[test]
fn system_install_invalid_name_and_unit_override_fail_before_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for name in ["../escape", "foo.socket", "foo@", "-foo", "a*b"] {
        let cfg = config(tmp.path(), "invalid", &format!("unit_name = \"{name}\""));
        for args in [&["install"][..], &["unit", "--name", "valid.service"][..]] {
            let output = cli(&home, &cfg, args);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("systemd.unit_name"));
            assert!(!tmp.path().join("invalid.token").exists());
            assert!(!home.exists());
        }
    }
    let cfg = config(
        tmp.path(),
        "explicit-old",
        "unit_name = \"foo.socket\"\nunit_path = \"\"",
    );
    let output = cli(&home, &cfg, &["unit", "--name", "valid.service"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Description=valid.service"));
    assert!(!tmp.path().join("explicit-old.token").exists());
}

#[cfg(unix)]
#[test]
fn system_install_readonly_write_failure_can_leave_token() {
    use std::os::unix::fs::PermissionsExt;
    // Root bypasses file mode permissions; CI and the shared development host use a non-root UID.
    if unsafe { libc::geteuid() == 0 } {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for scope in ["user", "system"] {
        let target = tmp.path().join(format!("readonly-{scope}.service"));
        fs::write(&target, "read only original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
        let cfg = config(
            tmp.path(),
            scope,
            &format!(
                "scope = \"{scope}\"\nunit_path = \"{}\"\nuser = \"neige-test\"\nhome = \"{}\"",
                target.display(),
                home.display()
            ),
        );
        let output = cli(&home, &cfg, &["install", "--force"]);
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("Permission denied"), "{err}");
        assert!(err.contains("appropriate permissions"));
        assert!(err.contains("preserving all arguments"));
        assert!(err.contains(cfg.to_str().unwrap()));
        assert!(err.contains(target.to_str().unwrap()));
        assert!(!err.contains("--force"));
        assert_eq!(fs::read(&target).unwrap(), b"read only original");
        let token = tmp.path().join(format!("{scope}.token"));
        assert_eq!(fs::read_to_string(&token).unwrap().trim().len(), 64);
        assert_eq!(
            fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn system_install_create_failure_preserves_os_reason_without_permission_advice() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let parent = tmp.path().join("not-directory");
    fs::write(&parent, "keep parent").unwrap();
    for scope in ["user", "system"] {
        let target = parent.join("unit.service");
        let cfg = config(
            tmp.path(),
            scope,
            &format!("scope = \"{scope}\"\nunit_path = \"{}\"", target.display()),
        );
        let output = cli(&home, &cfg, &["install"]);
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains(parent.to_str().unwrap()), "{err}");
        assert!(err.contains(cfg.to_str().unwrap()));
        assert!(
            err.contains("File exists") || err.contains("Not a directory"),
            "{err}"
        );
        assert!(!err.contains("appropriate permissions"));
        assert!(!err.contains("--force"));
        assert!(!tmp.path().join(format!("{scope}.token")).exists());
        assert_eq!(fs::read(&parent).unwrap(), b"keep parent");
    }
}
