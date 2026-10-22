//! A fresh, self-contained installation. Existing roots are never adopted.

#[cfg(unix)]
mod credentials;

use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use clap::Args;

use crate::config::AppConfig;
use crate::installed::{InstalledState, write_installed_state};
use crate::package::{release_executables, validate_installable_release};
use crate::preflight::PreflightMode;
use crate::upgrade;

#[derive(Args, Debug)]
pub(crate) struct SetupArgs {
    /// Extracted release directory containing manifest.json and bin/.
    #[arg(long)]
    package: PathBuf,
    /// Fresh installation directory. Defaults to ~/.local/share/neige-app.
    #[arg(long)]
    root: Option<PathBuf>,
    /// New config path inside the installation root. Defaults to ROOT/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Browser listen address; use 0.0.0.0 only for an intentional network deployment.
    #[arg(long, default_value = "127.0.0.1:4040")]
    listen: SocketAddr,
    /// Loopback admin listen address; must use a different port.
    #[arg(long, default_value = "127.0.0.1:4050")]
    admin_listen: SocketAddr,
    /// Owner login name.
    #[arg(long, default_value = "owner")]
    username: String,
    /// Private password file (mode 600); omit to choose a password interactively.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// Print machine-readable installation details instead of the guided next steps.
    #[arg(long)]
    json: bool,
}

#[cfg(not(unix))]
pub(crate) fn run(_args: SetupArgs) -> anyhow::Result<()> {
    anyhow::bail!("packaged setup currently requires Unix");
}

#[cfg(unix)]
pub(crate) fn run(args: SetupArgs) -> anyhow::Result<()> {
    // Validate every prerequisite before creating even the installation parents.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context("HOME must be an absolute directory")?;
    if args.root.is_none() {
        for existing in [
            home.join(".config/neige-app/config.toml"),
            home.join(".local/share/neige-calm"),
        ] {
            match fs::symlink_metadata(&existing) {
                Ok(_) => anyhow::bail!(
                    "existing installation found at {}; use its upgrade workflow or choose a separate --root",
                    existing.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("inspect existing installation before setup");
                }
            }
        }
    }
    let root = args
        .root
        .clone()
        .unwrap_or_else(|| home.join(".local/share/neige-app"));
    require_absent(&root)?;
    validate_path(&root)?;
    std::env::join_paths([root.join("bin")]).context(
        "installation bin directory cannot be represented in PATH; choose a different --root",
    )?;
    let config = args
        .config
        .clone()
        .unwrap_or_else(|| root.join("config.toml"));
    validate_path(&config)?;
    if config == root || !config.starts_with(&root) {
        anyhow::bail!(
            "--config must be inside installation root {}",
            root.display()
        );
    }
    // Reserve all owned paths; configuration must not collide with generated runtime files.
    let relative = config.strip_prefix(&root)?;
    if relative.components().next().is_some_and(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(
                "bin"
                    | "releases"
                    | "data"
                    | "plugins"
                    | "backups"
                    | "source"
                    | "admin.token"
                    | "neige-app.service"
            )
        )
    }) {
        anyhow::bail!("--config overlaps an installation runtime path");
    }
    if args.listen.port() == 0
        || args.admin_listen.port() == 0
        || args.listen.port() == args.admin_listen.port()
        || !args.admin_listen.ip().is_loopback()
    {
        anyhow::bail!(
            "listen ports must be nonzero and distinct, and admin-listen must be loopback-only"
        );
    }
    if args.username.trim().is_empty() || args.username.chars().any(char::is_control) {
        anyhow::bail!("username must be nonempty and contain no control characters");
    }
    let package = fs::canonicalize(&args.package).context("resolve extracted package directory")?;
    let manifest = upgrade::verify_v2_package_integrity(&package)?;
    validate_installable_release(&package, &manifest)?;
    let password = credentials::password(args.password_file.as_deref())?;
    let config_text = render_config(&root, &args, &password);
    let cfg = AppConfig::from_text(config.clone(), &config_text)?;
    validate_runtime_socket_paths(&cfg)?;

    // Only an exclusive mkdir grants rollback ownership. No existing file or
    // directory is removed, including when setup loses a concurrent mkdir race.
    let mut install = FreshRoot::create(root.clone())?;
    fs::create_dir_all(config.parent().context("config needs a parent")?)?;
    private_file(&config, config_text.as_bytes())?;
    crate::ensure_admin_token_file(Some(&root.join("admin.token")))?;
    let staged = upgrade::stage_upgrade(&cfg, &package, PreflightMode::Bundle)?;
    // Revalidate the staged identities before committing any active links.
    let staged_manifest = upgrade::verify_v2_package_integrity(&staged.stage_dir)?;
    validate_installable_release(&staged.stage_dir, &staged_manifest)?;
    if serde_json::to_value(&manifest)? != serde_json::to_value(&staged_manifest)? {
        anyhow::bail!(
            "package manifest changed during setup; retry from a stable extracted release"
        );
    }
    fs::create_dir_all(root.join("bin"))?;
    fs::create_dir_all(root.join("data"))?;
    fs::create_dir_all(root.join("plugins/config"))?;
    fs::create_dir_all(root.join("plugins/data"))?;
    for role in ["current-server", "current-web"] {
        std::os::unix::fs::symlink(&staged.stage_dir, cfg.release.root.join(role))?;
    }
    for (name, _) in release_executables() {
        std::os::unix::fs::symlink(
            cfg.release.current_server.join("bin").join(name),
            root.join("bin").join(name),
        )?;
    }
    write_installed_state(
        &cfg.calm_data_dir_resolved(),
        &InstalledState::from_manifest(&staged_manifest),
    )?;
    install.committed = true;
    let browser_address = if args.listen.ip().is_unspecified() {
        SocketAddr::new(
            if args.listen.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            },
            args.listen.port(),
        )
    } else {
        args.listen
    };
    let start = format!(
        "PATH={}:\"$PATH\" {} system serve --config {}",
        shell_quote(&root.join("bin")),
        shell_quote(&root.join("bin/neige-app")),
        shell_quote(&config)
    );
    let next_steps = [
        start,
        format!("Open http://{browser_address} and sign in with your chosen owner password."),
        include_str!("../../templates/system-setup-planner.md")
            .trim_end()
            .to_owned(),
        "Keep this terminal open; Ctrl-C stops Neige. Run the same foreground command to resume."
            .to_owned(),
    ];
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "installed": true,
                "root": root,
                "config": config,
                "releaseId": manifest.release_id,
                "username": args.username,
                "url": format!("http://{browser_address}"),
                "nextSteps": next_steps,
            }))?
        );
    } else {
        println!("Installed Neige release {}.", manifest.release_id);
        println!("Installation: {}", root.display());
        println!("Configuration: {}", config.display());
        println!("Owner login: {}", args.username);
        println!(
            "\nStart Neige by copying this command into your terminal:\n\n{}\n",
            next_steps[0]
        );
        for step in &next_steps[1..] {
            println!("{step}");
        }
    }
    Ok(())
}

fn validate_path(path: &Path) -> anyhow::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || path
            .to_str()
            .is_none_or(|text| text.chars().any(char::is_control))
    {
        anyhow::bail!(
            "setup paths must be absolute, UTF-8, and contain no parent traversal or control characters"
        );
    }
    // Configuration currently derives a SQLite URI directly from the data
    // directory. Reject URI syntax rather than let SQLx reinterpret that path.
    if path.to_str().is_some_and(|text| text.contains(['%', '?'])) {
        anyhow::bail!("setup paths must not contain '%' or '?'; choose a literal filesystem path");
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("setup path {} contains a symlink", ancestor.display())
            }
            Ok(metadata) if ancestor != path && !metadata.is_dir() => anyhow::bail!(
                "setup path parent {} is not a directory",
                ancestor.display()
            ),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", ancestor.display()));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_runtime_socket_paths(cfg: &AppConfig) -> anyhow::Result<()> {
    for socket in [
        cfg.proc_supervisor_sock(),
        cfg.calm_mcp_kernel_sock(),
        calm_types::runtime_paths::shared_codex_appserver_socket(&cfg.calm_data_dir_resolved()),
    ] {
        std::os::unix::net::SocketAddr::from_pathname(&socket).with_context(|| {
            format!(
                "installation root is too long for Unix socket {}; choose a shorter --root",
                socket.display()
            )
        })?;
    }
    if let Some(tailnet) = &cfg.tailnet {
        tailnet.validate(&cfg.child.extra_args)?;
    }
    Ok(())
}

fn require_absent(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!(
            "installation root {} already exists; use its upgrade workflow or choose a fresh --root",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(unix)]
struct FreshRoot {
    root: PathBuf,
    parents: Vec<PathBuf>,
    committed: bool,
}

#[cfg(unix)]
impl FreshRoot {
    fn create(root: PathBuf) -> anyhow::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let mut parents = Vec::new();
        for parent in root
            .parent()
            .context("installation root needs a parent")?
            .ancestors()
        {
            if parent.exists() {
                break;
            }
            parents.push(parent.to_path_buf());
        }
        let mut created = Vec::new();
        for parent in parents
            .into_iter()
            .rev()
            .chain(std::iter::once(root.clone()))
        {
            if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&parent) {
                for owned in created.iter().rev() {
                    let _ = fs::remove_dir(owned);
                }
                return Err(error).with_context(|| {
                    format!(
                        "exclusively create fresh installation directory {}",
                        parent.display()
                    )
                });
            }
            created.push(parent);
        }
        created.pop();
        Ok(Self {
            root,
            parents: created,
            committed: false,
        })
    }
}

#[cfg(unix)]
impl Drop for FreshRoot {
    fn drop(&mut self) {
        if !self.committed {
            if let Err(error) = fs::remove_dir_all(&self.root) {
                tracing::warn!(path = %self.root.display(), %error, "remove incomplete fresh installation");
            }
            for parent in self.parents.iter().rev() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

#[cfg(unix)]
fn private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn render_config(root: &Path, args: &SetupArgs, password: &str) -> String {
    let mut text = String::from(
        "# Generated by neige-app system setup. Contains private login credentials.\n",
    );
    for (section, values) in [
        (
            "admin",
            vec![
                ("listen", args.admin_listen.to_string()),
                ("token_file", root.join("admin.token").display().to_string()),
            ],
        ),
        (
            "release",
            vec![
                ("root", root.join("releases").display().to_string()),
                ("backups", root.join("backups").display().to_string()),
            ],
        ),
        (
            "child",
            vec![
                (
                    "bin",
                    root.join("releases/current-server/bin/calm-server")
                        .display()
                        .to_string(),
                ),
                (
                    "proc_supervisor_bin",
                    root.join("releases/current-server/bin/calm-proc-supervisor")
                        .display()
                        .to_string(),
                ),
                (
                    "fe_dist",
                    root.join("releases/current-web/web/dist/next")
                        .display()
                        .to_string(),
                ),
                ("calm_listen", args.listen.to_string()),
                ("data_dir", root.join("data").display().to_string()),
                (
                    "plugins_dir",
                    root.join("plugins/config").display().to_string(),
                ),
                (
                    "plugins_data_dir",
                    root.join("plugins/data").display().to_string(),
                ),
                (
                    "mcp_stdio_shim_bin",
                    root.join("releases/current-server/bin/neige-mcp-stdio-shim")
                        .display()
                        .to_string(),
                ),
                ("auth_username", args.username.clone()),
                ("auth_password", password.to_owned()),
                ("cwd", root.join("data").display().to_string()),
            ],
        ),
        ("tailnet", vec![("provider", "private-tailnet".to_owned())]),
        (
            "systemd",
            vec![
                ("unit_name", "neige-app".to_owned()),
                ("bin", root.join("bin/neige-app").display().to_string()),
                (
                    "unit_path",
                    root.join("neige-app.service").display().to_string(),
                ),
            ],
        ),
        (
            "source",
            vec![("checkout_dir", root.join("source").display().to_string())],
        ),
    ] {
        text.push_str(&format!("\n[{section}]\n"));
        for (key, value) in values {
            text.push_str(&format!("{key} = {}\n", quoted(&value)));
        }
        if section == "child" {
            text.push_str("auth_dev_autologin = false\nextra_args = []\n");
        }
    }
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn exclusive_root_creation_preserves_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("existing");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep"), "original").unwrap();
        assert!(FreshRoot::create(root.clone()).is_err());
        assert_eq!(fs::read(root.join("keep")).unwrap(), b"original");
    }

    #[test]
    fn incomplete_install_rolls_back_only_owned_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("new-parent/installation");
        let sibling = temp.path().join("keep");
        fs::write(&sibling, "original").unwrap();
        {
            let _install = FreshRoot::create(root.clone()).unwrap();
            fs::write(root.join("partial"), "new file").unwrap();
        }
        assert!(!root.parent().unwrap().exists());
        assert_eq!(fs::read(sibling).unwrap(), b"original");
    }

    #[test]
    fn generated_config_roundtrips_password_and_isolated_paths() {
        let root = PathBuf::from("/tmp/Neige # with \\\"quotes'");
        let args = SetupArgs {
            package: PathBuf::from("/package"),
            root: Some(root.clone()),
            config: None,
            listen: "127.0.0.1:4040".parse().unwrap(),
            admin_listen: "127.0.0.1:4050".parse().unwrap(),
            username: "owner".to_owned(),
            password_file: None,
            json: false,
        };
        let password = "private # password \\\" with punctuation";
        let cfg = AppConfig::from_text(
            root.join("config.toml"),
            &render_config(&root, &args, password),
        )
        .unwrap();
        assert_eq!(cfg.child.auth_password.as_deref(), Some(password));
        assert_eq!(cfg.child.data_dir, Some(root.join("data")));
        assert_eq!(cfg.child.plugins_dir, Some(root.join("plugins/config")));
        assert_eq!(cfg.child.plugins_data_dir, Some(root.join("plugins/data")));
        assert_eq!(cfg.source.checkout_dir, root.join("source"));
        assert!(!cfg.child.auth_dev_autologin);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_socket_validation_includes_shared_codex_listener() {
        // On Linux, the longer shared listener exceeds sun_path while the
        // supervisor and MCP listeners still fit. Tailnet is independently optional.
        let data = PathBuf::from(format!("/{}", "x".repeat(83)));
        let mut cfg = AppConfig::starter(PathBuf::from("/config.toml"));
        cfg.child.data_dir = Some(data);
        cfg.tailnet = None;
        for path in [cfg.proc_supervisor_sock(), cfg.calm_mcp_kernel_sock()] {
            assert!(std::os::unix::net::SocketAddr::from_pathname(path).is_ok());
        }
        let error = validate_runtime_socket_paths(&cfg).unwrap_err();
        assert!(error.to_string().contains("codex-appserver.sock"));
    }
}

#[cfg(test)]
mod quote_contract_tests {
    #[test]
    fn posix_quote_preserves_command_bytes() {
        for (input, expected) in [
            ("", "''"),
            ("plain", "'plain'"),
            ("two words", "'two words'"),
            ("a'b", "'a'\\''b'"),
            ("''", "''\\'''\\'''"),
            ("$`\\\n", "'$`\\\n'"),
        ] {
            assert_eq!(
                super::shell_quote(std::path::Path::new(input)),
                expected,
                "{input:?}"
            );
        }
    }
}
