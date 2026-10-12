//! The systemd unit templates: the directives the design needs, every
//! placeholder replaced by the documented substitution, and no SDK variable.
//! Only lint: no unit is installed or started.

use std::{collections::BTreeSet, path::Path};

const UNITS: [&str; 3] = [
    "research-archive-capture.service",
    "research-archive-capture.timer",
    "research-archive-notify@.service",
];

/// The README's substitution.
const PLACEHOLDERS: [(&str, &str); 5] = [
    ("@BIN@", "/opt/ra/bin/research-archive"),
    ("@CONFIG@", "/opt/ra/research-archive.toml"),
    ("@DATA_DIR@", "/opt/ra/data"),
    ("@TIMEOUT_SEC@", "1800"),
    ("@PROXY_ENV_FILE@", "/opt/ra/proxy.env"),
];

fn template(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("systemd")
            .join(name),
    )
    .unwrap()
}

fn render(text: &str) -> String {
    PLACEHOLDERS
        .iter()
        .fold(text.to_owned(), |t, (from, to)| t.replace(from, to))
}

/// The directive lines, without comments.
fn directives(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
        .map(str::to_owned)
        .collect()
}

#[test]
fn templates_use_only_the_documented_placeholders() {
    let mut used = BTreeSet::new();
    for name in UNITS {
        let text = template(name);
        let mut rest = text.as_str();
        while let Some(at) = rest.find('@') {
            let tail = &rest[at + 1..];
            match tail.find('@') {
                Some(end)
                    if tail[..end]
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b == b'_')
                        && end > 0 =>
                {
                    used.insert(format!("@{}@", &tail[..end]));
                    rest = &tail[end + 1..];
                }
                _ => rest = tail,
            }
        }
        let rendered = render(&text);
        for (placeholder, _) in PLACEHOLDERS {
            assert!(!rendered.contains(placeholder), "{name}");
        }
    }
    let documented: BTreeSet<_> = PLACEHOLDERS.iter().map(|(p, _)| (*p).to_owned()).collect();
    assert_eq!(used, documented);
}

#[test]
fn no_template_sets_an_sdk_variable_or_any_environment() {
    for name in UNITS {
        let text = template(name);
        assert!(!text.contains("LONGBRIDGE_"), "{name}");
        assert!(!text.contains("LONGPORT_"), "{name}");
        for line in directives(&text) {
            assert!(!line.starts_with("Environment="), "{name}: {line}");
            assert!(!line.starts_with("EnvironmentFile="), "{name}: {line}");
            assert!(!line.starts_with("PassEnvironment="), "{name}: {line}");
        }
    }
}

#[test]
fn the_capture_service_is_a_bounded_oneshot_that_alerts_on_failure() {
    let lines = directives(&render(&template("research-archive-capture.service")));
    for expected in [
        "Type=oneshot",
        "WorkingDirectory=/opt/ra/data",
        "ExecStart=/opt/ra/bin/research-archive capture --config /opt/ra/research-archive.toml",
        "TimeoutStartSec=1800",
        "OnFailure=research-archive-notify@%n.service",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "missing {expected}: {lines:?}"
        );
    }
}

#[test]
fn the_timer_fires_every_half_hour_and_catches_up() {
    let lines = directives(&template("research-archive-capture.timer"));
    for expected in [
        "OnCalendar=*-*-* *:00,30:00",
        "Persistent=true",
        "Unit=research-archive-capture.service",
        "WantedBy=timers.target",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "missing {expected}: {lines:?}"
        );
    }
}

#[test]
fn the_notify_service_passes_the_failed_unit() {
    let lines = directives(&render(&template("research-archive-notify@.service")));
    for expected in [
        "Type=oneshot",
        "WorkingDirectory=/opt/ra/data",
        "ExecStart=/opt/ra/bin/research-archive notify --config /opt/ra/research-archive.toml --failed-unit %i",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "missing {expected}: {lines:?}"
        );
    }
}
