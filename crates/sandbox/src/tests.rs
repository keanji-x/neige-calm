use super::*;

fn strings(args: &[OsString]) -> Vec<&str> {
    args.iter().map(|a| a.to_str().unwrap()).collect()
}

const BASE: [&str; 27] = [
    "--unshare-all",
    "--share-net",
    "--die-with-parent",
    "--new-session",
    "--ro-bind",
    "/usr",
    "/usr",
    "--symlink",
    "usr/bin",
    "/bin",
    "--symlink",
    "usr/sbin",
    "/sbin",
    "--symlink",
    "usr/lib",
    "/lib",
    "--symlink",
    "usr/lib64",
    "/lib64",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--tmpfs",
    "/tmp",
    "--tmpfs",
    "/dev/shm",
];

#[test]
fn an_empty_policy_is_the_system_base_and_nothing_else() {
    let policy = Policy::new("/usr/bin/bwrap").unwrap();
    assert_eq!(policy.program(), Path::new("/usr/bin/bwrap"));
    let args = policy.args();
    let mut expected = BASE.to_vec();
    expected.push("--");
    assert_eq!(strings(&args), expected);
}

/// `/etc` is a closed allowlist: each entry is one read-only bind of the
/// same path, missing entries are skipped, and nothing else of `/etc` appears.
#[test]
fn etc_is_the_closed_allowlist() {
    assert_eq!(
        Etc::ALL.map(Etc::path),
        [
            "/etc/fonts",
            "/etc/ssl/certs",
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/localtime",
            "/etc/ld.so.cache",
        ]
    );
    let mut policy = Policy::new("/usr/bin/bwrap").unwrap();
    // Added out of order and twice: the argv is still the enum's order, once.
    for entry in Etc::ALL.iter().rev().chain(Etc::ALL.iter()) {
        policy = policy.etc(*entry);
    }
    let args = policy.args();
    let args = strings(&args);
    let etc: Vec<&[&str]> = args[BASE.len()..args.len() - 1].chunks(3).collect();
    let expected: Vec<[&str; 3]> = Etc::ALL
        .iter()
        .map(|e| ["--ro-bind-try", e.path(), e.path()])
        .collect();
    assert_eq!(etc, expected.iter().map(|e| &e[..]).collect::<Vec<_>>());
    let mentions = args.iter().filter(|a| a.starts_with("/etc")).count();
    assert_eq!(mentions, 2 * Etc::ALL.len());
}

#[test]
fn binds_follow_the_base_in_order_and_end_with_the_separator() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-0");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let policy = Policy::new("/usr/bin/bwrap")
        .unwrap()
        .etc(Etc::Hosts)
        .read_only("/opt/google/chrome", "/opt/chrome")
        .unwrap()
        .read_write("/srv/data/profile", "/data/profile")
        .unwrap()
        .socket(&socket, "/run/wayland/wayland-0")
        .unwrap()
        .env("TZ", "UTC")
        .unwrap();
    let args = policy.args();
    let args = strings(&args);
    let socket = socket.to_str().unwrap();
    assert_eq!(
        &args[BASE.len()..],
        [
            "--ro-bind-try",
            "/etc/hosts",
            "/etc/hosts",
            "--ro-bind",
            "/opt/google/chrome",
            "/opt/chrome",
            "--bind",
            "/srv/data/profile",
            "/data/profile",
            "--ro-bind",
            socket,
            "/run/wayland/wayland-0",
            "--setenv",
            "TZ",
            "UTC",
            "--",
        ]
    );
}

#[test]
fn paths_are_normalised_and_bad_ones_refused() {
    let policy = Policy::new("/usr/bin/bwrap")
        .unwrap()
        .read_only("/opt//chrome/./", "/opt/./chrome")
        .unwrap();
    let args = policy.args();
    assert_eq!(
        strings(&args[BASE.len()..]),
        ["--ro-bind", "/opt/chrome", "/opt/chrome", "--"]
    );
    assert!(matches!(
        Policy::new("bwrap"),
        Err(Error::BadPath { what: "bwrap", .. })
    ));
    let base = || Policy::new("/usr/bin/bwrap").unwrap();
    for (host, inside) in [
        ("relative/dir", "/data/x"),
        ("/srv/../etc/shadow", "/data/x"),
        ("/srv/data", "data/x"),
        ("/srv/data", "/data/../usr"),
    ] {
        assert!(
            matches!(base().read_only(host, inside), Err(Error::BadPath { .. })),
            "{host} -> {inside}"
        );
    }
}

/// The host's root and `/etc` never reach the sandbox through a bind, and a
/// bind never covers the system base.
#[test]
fn binds_cannot_escape_the_base_or_the_etc_allowlist() {
    let base = || Policy::new("/usr/bin/bwrap").unwrap();
    for host in ["/", "/etc", "/etc/shadow", "/etc/ssh"] {
        assert!(
            matches!(
                base().read_only(host, "/data/x"),
                Err(Error::HostRefused(_))
            ),
            "{host}"
        );
        assert!(
            matches!(
                base().read_write(host, "/data/x"),
                Err(Error::HostRefused(_))
            ),
            "{host}"
        );
    }
    // Lexical, component-wise: a sibling named like /etc is fine.
    assert!(base().read_only("/etcetera", "/data/x").is_ok());
    for inside in [
        "/",
        "/usr",
        "/usr/lib/x",
        "/bin",
        "/lib64",
        "/proc/self",
        "/dev/shm",
        "/tmp/x",
        "/etc",
        "/etc/passwd",
    ] {
        assert!(
            matches!(base().read_only("/srv/x", inside), Err(Error::Reserved(_))),
            "{inside}"
        );
    }
    assert!(base().read_only("/srv/x", "/tmpdata").is_ok());
    let twice = base().read_only("/srv/a", "/data/x").unwrap();
    assert!(matches!(
        twice.read_write("/srv/b", "/data/x"),
        Err(Error::Duplicate(_))
    ));
}

#[test]
fn a_socket_bind_needs_a_socket() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain");
    std::fs::write(&file, b"").unwrap();
    let base = || Policy::new("/usr/bin/bwrap").unwrap();
    assert!(matches!(
        base().socket(&file, "/run/s"),
        Err(Error::NotASocket(_))
    ));
    assert!(matches!(
        base().socket(dir.path().join("missing"), "/run/s"),
        Err(Error::Inspect { .. })
    ));
}

#[test]
fn environment_names_are_checked() {
    let base = || Policy::new("/usr/bin/bwrap").unwrap();
    for bad in ["", "A=B", "A\0"] {
        assert!(
            matches!(base().env(bad, "v"), Err(Error::EnvName(_))),
            "{bad:?}"
        );
    }
}
