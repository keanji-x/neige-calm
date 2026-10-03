use super::*;

fn load(scope: &str, name: Option<&str>, unit_path: Option<&str>) -> anyhow::Result<AppConfig> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("config.toml");
    let mut text = format!("[systemd]\nscope = \"{scope}\"\n");
    for (key, value) in [("unit_name", name), ("unit_path", unit_path)] {
        if let Some(value) = value {
            text.push_str(&format!(
                "{key} = \"{}\"\n",
                value.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
    }
    fs::write(&path, text)?;
    AppConfig::load(Some(&path))
}

#[test]
fn systemd_default_path_uses_final_name_and_scope() {
    for scope in ["user", "system"] {
        let directory = if scope == "user" {
            home_dir().join(".config/systemd/user")
        } else {
            PathBuf::from("/etc/systemd/system")
        };
        for (name, filename) in [
            (None, "neige-app.service"),
            (Some("neige-app"), "neige-app.service"),
            (Some("neige-app.service"), "neige-app.service"),
            (Some("neige.worker"), "neige.worker.service"),
            (Some("neige@blue"), "neige@blue.service"),
            (Some("neige@blue.service"), "neige@blue.service"),
            (Some("foo.socket.service"), "foo.socket.service"),
            (Some("foo.service.service"), "foo.service.service"),
            (Some("A_1-:b"), "A_1-:b.service"),
        ] {
            let cfg = load(scope, name, None).unwrap();
            assert_eq!(cfg.systemd.unit_path, directory.join(filename));
            assert_eq!(cfg.systemd.unit_name, name.unwrap_or("neige-app"));
        }
    }
}

#[test]
fn systemd_derived_name_rejects_invalid_stems_and_types() {
    for name in [
        "",
        ".",
        "..",
        ".service",
        "..service",
        "...service",
        "-foo",
        "-foo.service",
        "../x",
        "/x",
        "x/y",
        "x\\y",
        "foo\\x20bar",
        "a b",
        "a\tb",
        "a\rb",
        "a\0b",
        "a$b",
        "a*b",
        "a?b",
        "a[b",
        "a;b",
        "a`b",
        "a%b",
        "雪",
        "@blue",
        "foo@",
        "foo@.service",
        "foo@@blue",
        "foo@blue@red",
        "foo.socket",
        "foo.target",
        "foo.device",
        "foo.mount",
        "foo.automount",
        "foo.swap",
        "foo.timer",
        "foo.path",
        "foo.slice",
        "foo.scope",
    ] {
        for scope in ["user", "system"] {
            let err = load(scope, Some(name), None).unwrap_err();
            assert!(
                err.to_string().contains("systemd.unit_name"),
                "{name:?}: {err:#}"
            );
        }
    }
}

#[test]
fn systemd_filename_length_boundaries() {
    for scope in ["user", "system"] {
        for suffix in ["", ".service"] {
            let valid = format!("{}{suffix}", "a".repeat(247));
            let cfg = load(scope, Some(&valid), None).unwrap();
            assert_eq!(cfg.systemd.unit_path.file_name().unwrap().len(), 255);
            assert_eq!(cfg.systemd.unit_name, valid);
            let invalid = format!("{}{suffix}", "a".repeat(248));
            assert!(
                load(scope, Some(&invalid), None)
                    .unwrap_err()
                    .to_string()
                    .contains("255 bytes")
            );
        }
    }
}

#[test]
fn systemd_explicit_path_keeps_original_loading_semantics() {
    for scope in ["user", "system"] {
        for path in [
            "",
            "relative.service",
            "~",
            "~/old.service",
            "/tmp/old.service",
        ] {
            let expected = match path {
                "~" => home_dir(),
                "~/old.service" => home_dir().join("old.service"),
                other => PathBuf::from(other),
            };
            for name in [
                "",
                "../old",
                "foo\\x20bar",
                "-foo",
                "foo.socket",
                "foo@",
                "雪",
                "different.service",
            ] {
                let cfg = load(scope, Some(name), Some(path)).unwrap();
                assert_eq!(cfg.systemd.unit_path, expected);
                assert_eq!(cfg.systemd.unit_name, name);
            }
        }
    }
}
