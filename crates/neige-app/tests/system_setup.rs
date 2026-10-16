#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BINARIES: &[&str] = &[
    "calm-server",
    "calm-proc-supervisor",
    "neige-codex-bridge",
    "neige-mcp-stdio-shim",
    "neige-tailnet",
    "neige",
];

struct Fixture {
    temp: tempfile::TempDir,
    package: PathBuf,
    home: PathBuf,
    root: PathBuf,
    password_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let home = base.join("home");
        fs::create_dir(&home).unwrap();
        let source = base.join("source");
        fs::create_dir_all(source.join("frontend")).unwrap();
        fs::write(source.join("frontend/index.html"), "<html>Neige</html>").unwrap();
        for name in BINARIES {
            let script = if *name == "calm-server" {
                r#"#!/bin/sh
case "$1" in
--version) echo 'calm-server 0.1.0';;
--emit-kernel-compatibility-json) echo '{"terminalFrameVersion":4,"terminalProtocolVersion":4,"apiVersion":"1","syncEventVersion":1,"mcpProtocolVersion":"2024-11-05","pluginMcpProtocolVersion":"2025-11-25","webCompatVersion":2,"minWebCompatVersion":2,"supervisorControlVersion":1}';;
*) exit 2;;
esac
"#
                .to_owned()
            } else {
                format!("#!/bin/sh\necho '{name} 0.1.0'\n")
            };
            let path = source.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let package = base.join("package");
        let mut command = isolated_cli(&home);
        command
            .args(["system", "package", "--release-id", "setup-fixture"])
            .arg("--release-dir")
            .arg(&package)
            .arg("--app-bin")
            .arg(env!("CARGO_BIN_EXE_neige-app"))
            .arg("--fe-dist")
            .arg(source.join("frontend"));
        for name in BINARIES {
            command
                .arg("--bin")
                .arg(format!("{name}={}", source.join(name).display()));
        }
        success(&command.output().unwrap());
        let password_file = base.join("password");
        fs::write(&password_file, "test password # with \\\" punctuation\n").unwrap();
        fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            root: base.join("installation"),
            temp,
            package,
            home,
            password_file,
        }
    }

    fn setup(&self, extra: &[&str]) -> Output {
        isolated_cli(&self.home)
            .args(["system", "setup"])
            .arg("--package")
            .arg(&self.package)
            .arg("--root")
            .arg(&self.root)
            .arg("--password-file")
            .arg(&self.password_file)
            .args(extra)
            .output()
            .unwrap()
    }
}

fn isolated_cli(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_neige-app"));
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin");
    command
}

fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn refusal(output: &Output, expected: &str) {
    assert!(!output.status.success(), "unexpected success");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains(expected), "expected {expected:?}: {err}");
    assert!(!err.contains("test password"), "credential leaked: {err}");
}

#[test]
fn setup_default_output_has_a_copyable_command_for_quoted_paths() {
    let mut fixture = Fixture::new();
    fixture.root = fixture.temp.path().join("install root's");
    let config = fixture.root.join("config \"owner\".toml");
    let output = fixture.setup(&["--config", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let base = fixture.temp.path().display();
    let expected_command = format!(
        "PATH='{base}/install root'\\''s/bin':\"$PATH\" '{base}/install root'\\''s/bin/neige-app' system serve --config '{base}/install root'\\''s/config \"owner\".toml'"
    );
    assert!(
        stdout.lines().any(|line| line == expected_command),
        "command is not directly copyable: {stdout}"
    );
    for expected in [
        "http://127.0.0.1:4040",
        "Settings",
        "Planners",
        "Ready",
        "Keep this terminal open",
        "Ctrl-C",
        "owner",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    assert!(stdout.contains(fixture.root.to_str().unwrap()));
    assert!(stdout.contains(config.to_str().unwrap()));
    assert!(!stdout.trim_start().starts_with('{'));
    let token = fs::read_to_string(fixture.root.join("admin.token")).unwrap();
    for text in [&stdout, &stderr] {
        assert!(!text.contains("test password"), "credential leaked");
        assert!(!text.contains(&token), "admin token leaked");
    }
}

#[test]
fn setup_creates_private_isolated_install_and_foreground_command() {
    let fixture = Fixture::new();
    let output = fixture.setup(&[
        "--json",
        "--listen",
        "127.0.0.1:14040",
        "--admin-listen",
        "127.0.0.1:14050",
    ]);
    let result = success(&output);
    let config = fixture.root.join("config.toml");
    assert_eq!(result["config"], config.to_str().unwrap());
    assert_eq!(result["url"], "http://127.0.0.1:14040");
    assert_eq!(result["username"], "owner");
    let next_steps = result["nextSteps"].to_string();
    assert!(next_steps.contains("system serve --config"));
    assert!(next_steps.contains("Settings"));
    assert!(!next_steps.contains("systemctl"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test password"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("test password"));
    for path in [&config, &fixture.root.join("admin.token")] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&fixture.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("admin.token"))
            .unwrap()
            .len(),
        64
    );
    let config_text = fs::read_to_string(config).unwrap();
    for (key, relative) in [
        ("data_dir", "data"),
        ("plugins_dir", "plugins/config"),
        ("plugins_data_dir", "plugins/data"),
        ("cwd", "data"),
    ] {
        assert!(config_text.contains(&format!(
            "{key} = \"{}\"",
            fixture.root.join(relative).display()
        )));
    }
    assert!(config_text.contains("auth_dev_autologin = false"));
    assert!(config_text.contains("127.0.0.1:14050"));
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.package.join("manifest.json")).unwrap()).unwrap();
    let installed: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("data/state/installed.json")).unwrap())
            .unwrap();
    for key in ["releaseId", "productMajor", "compatibility"] {
        assert_eq!(installed[key], manifest[key]);
    }
    for (name, unit) in manifest["units"].as_object().unwrap() {
        assert_eq!(installed["units"][name]["version"], unit["version"]);
        assert_eq!(
            installed["units"][name]["binarySha256"],
            unit["binarySha256"]
        );
    }
    let staged = fixture.root.join("releases/staged/setup-fixture");
    for role in ["current-server", "current-web"] {
        assert_eq!(
            fs::read_link(fixture.root.join("releases").join(role)).unwrap(),
            staged
        );
    }
    for name in BINARIES.iter().copied().chain(["neige-app"]) {
        assert!(
            fixture.root.join("bin").join(name).is_file(),
            "missing stable link {name}"
        );
    }
    let version = Command::new(fixture.root.join("bin/neige-app"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
}

#[test]
fn setup_rejects_tampered_package_before_writes() {
    let fixture = Fixture::new();
    let path = fixture.package.join("bin/neige");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
    refusal(&fixture.setup(&[]), "sha256 mismatch");
    assert!(!fixture.root.exists());
}

#[test]
fn setup_rejects_incomplete_or_inconsistent_release_identity() {
    for missing_unit in [false, true] {
        let fixture = Fixture::new();
        let manifest_path = fixture.package.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        if missing_unit {
            manifest["units"]
                .as_object_mut()
                .unwrap()
                .remove("calmProcSupervisor");
        } else {
            manifest["units"]["neigeCli"]["binarySha256"] = Value::String("0".repeat(64));
        }
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        refusal(
            &fixture.setup(&[]),
            if missing_unit {
                "requires unit"
            } else {
                "unit identity"
            },
        );
        assert!(!fixture.root.exists());
    }
}

#[test]
fn setup_refuses_existing_root_and_dangling_link_without_overwrite() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.root).unwrap();
    fs::write(fixture.root.join("keep"), "original state").unwrap();
    refusal(&fixture.setup(&[]), "already exists");
    assert_eq!(
        fs::read(fixture.root.join("keep")).unwrap(),
        b"original state"
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
    fs::remove_file(fixture.root.join("keep")).unwrap();
    fs::remove_dir(&fixture.root).unwrap();
    let outside = fixture.temp.path().join("missing");
    symlink(&outside, &fixture.root).unwrap();
    refusal(&fixture.setup(&[]), "already exists");
    assert_eq!(fs::read_link(&fixture.root).unwrap(), outside);
    assert!(!outside.exists());
}

#[test]
fn setup_rejects_unsafe_configuration_before_writes() {
    let fixture = Fixture::new();
    let outside = fixture.temp.path().join("outside.toml");
    refusal(
        &fixture.setup(&["--config", outside.to_str().unwrap()]),
        "inside installation root",
    );
    assert!(!outside.exists());
    for extra in [
        vec!["--listen", "127.0.0.1:4050"],
        vec!["--admin-listen", "0.0.0.0:4050"],
        vec!["--listen", "127.0.0.1:0"],
    ] {
        refusal(&fixture.setup(&extra), "listen");
        assert!(!fixture.root.exists());
    }
    fs::set_permissions(&fixture.password_file, fs::Permissions::from_mode(0o644)).unwrap();
    refusal(&fixture.setup(&[]), "private");
    assert!(!fixture.root.exists());
}

#[test]
fn setup_rejects_uri_metacharacters_before_writes() {
    for name in ["encoded%2f..%2foutside", "query?mode=memory"] {
        let mut fixture = Fixture::new();
        fixture.root = fixture.temp.path().join(name);
        refusal(&fixture.setup(&[]), "must not contain '%' or '?'");
        assert!(!fixture.root.exists());
        assert!(!fixture.temp.path().join("outside").exists());
    }
    for name in ["config%2foutside.toml", "config?.toml"] {
        let fixture = Fixture::new();
        let config = fixture.root.join(name);
        refusal(
            &fixture.setup(&["--config", config.to_str().unwrap()]),
            "must not contain '%' or '?'",
        );
        assert!(!fixture.root.exists());
    }
}

#[test]
fn setup_rejects_unrepresentable_runtime_bin_path_before_writes() {
    let mut fixture = Fixture::new();
    let missing_parent = fixture.temp.path().join("p:c");
    fixture.root = missing_parent.join("app");
    refusal(&fixture.setup(&[]), "represented in PATH");
    assert!(!fixture.root.exists());
    assert!(!missing_parent.exists());

    fixture.root = fixture.temp.path().join("plain");
    let config = fixture.root.join("configuration:local.toml");
    assert!(
        fixture
            .setup(&["--config", config.to_str().unwrap()])
            .status
            .success(),
        "a colon in a configuration filename does not enter PATH"
    );
}

#[test]
fn setup_requires_owner_executable_package_binaries_before_writes() {
    // Keep owner-read permission so integrity verification can reach the
    // executable-mode check when the test runs without root privileges.
    for mode in [0o744, 0o410, 0o401] {
        let fixture = Fixture::new();
        let binary = fixture.package.join("bin/calm-server");
        fs::set_permissions(&binary, fs::Permissions::from_mode(mode)).unwrap();
        if mode == 0o744 {
            assert!(
                fixture.setup(&[]).status.success(),
                "owner-executable release should install"
            );
        } else {
            refusal(&fixture.setup(&[]), "owner-executable bin/calm-server");
            assert!(!fixture.root.exists());
        }
    }
}

#[test]
fn setup_rejects_long_runtime_socket_paths_before_writes() {
    let mut fixture = Fixture::new();
    fixture.root = fixture.temp.path().join("x".repeat(100));
    refusal(&fixture.setup(&[]), "too long for Unix socket");
    assert!(!fixture.root.exists());
    fixture.root = fixture.temp.path().join("short");
    assert!(
        fixture.setup(&[]).status.success(),
        "shortening --root must recover setup"
    );
}

#[test]
fn setup_refuses_default_legacy_installation_before_writes() {
    let fixture = Fixture::new();
    let legacy = fixture.home.join(".config/neige-app/config.toml");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, "existing configuration").unwrap();
    let output = isolated_cli(&fixture.home)
        .args(["system", "setup", "--package"])
        .arg(&fixture.package)
        .arg("--password-file")
        .arg(&fixture.password_file)
        .output()
        .unwrap();
    refusal(&output, "existing installation");
    assert_eq!(fs::read(&legacy).unwrap(), b"existing configuration");
    assert!(!fixture.home.join(".local/share/neige-app").exists());
}
