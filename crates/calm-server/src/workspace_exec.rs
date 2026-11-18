//! Load a workspace environment inside an already-admitted execution, then replace this process.
use clap::Parser;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

const MODE: &str = "--workspace-environment-exec";

#[derive(Parser)]
#[command(name = "calm-server", disable_version_flag = true)]
struct WorkspaceExec {
    #[arg(long = "workspace-environment-exec", hide = true)]
    _workspace_environment_exec: bool,
    #[arg(long)]
    source: PathBuf,
    /// Kernel-selected direnv configuration; omission excludes inherited supervisor selection.
    #[arg(long)]
    direnv_config: Option<PathBuf>,
    #[arg(long)]
    derived_root: Vec<PathBuf>,
    #[arg(long)]
    preserve_key: Vec<String>,
    #[arg(long)]
    preserve_prefix: Vec<String>,
    /// Keep selected HTTP/HTTPS groups from the admitted child's original environment.
    #[arg(long)]
    preserve_selected_proxies: bool,
    #[arg(long)]
    path_prefix: Option<PathBuf>,
    /// Private stdout pipe to the native shell hook, never a durable environment file.
    #[arg(long, conflicts_with = "target")]
    print_environment: bool,
    #[arg(last = true, required_unless_present = "print_environment", num_args = 1..)]
    target: Vec<OsString>,
}

/// Dispatch the private local mode before tracing, storage or service initialization.
/// A successful dispatch replaces this process or exits after a pipe export; runtime failures exit 75.
pub fn run_if_requested() {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(MODE)) {
        return;
    }
    let options = WorkspaceExec::parse();
    if let Err(error) = execute(options) {
        eprintln!("{error}");
        std::process::exit(75);
    }
    std::process::exit(0);
}

fn execute(options: WorkspaceExec) -> anyhow::Result<()> {
    anyhow::ensure!(
        options.source.is_absolute(),
        "workspace environment source must be absolute"
    );
    anyhow::ensure!(
        options.derived_root.iter().all(|root| root.is_absolute()),
        "workspace derived roots must be absolute"
    );
    let source = if options.print_environment || !options.derived_root.is_empty() {
        crate::workspace_service::environment::SourceMapping {
            source: options.source,
            derived_roots: options.derived_root,
        }
        .select(&std::env::current_dir()?)?
    } else {
        options.source
    };
    let original: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    // Project code sees the developer-tool base, never the provider or Neige
    // runtime credentials held by the admitted child.
    let mut base = crate::workspace_service::environment::directory_base_environment();
    base.remove(OsStr::new("DIRENV_CONFIG"));
    if let Some(config) = options.direnv_config {
        // Direnv interprets relative configuration paths in its source cwd, as
        // the kernel's direct directory resolver does, never the supervisor cwd.
        base.insert("DIRENV_CONFIG".into(), config.into_os_string());
    }
    let mut environment = workspace::environment::resolve(&source, base)?.into_values();
    let mut keys = options
        .preserve_key
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if options.preserve_selected_proxies {
        // The supervisor may have different defaults from the kernel. Select
        // groups from this actual child, without exposing their values to project code.
        keys.extend(
            crate::proxy_env::resolved_proxy_env_pairs(None, None, |key| {
                original
                    .get(OsStr::new(key))
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            })
            .into_iter()
            .flat_map(|(upper, lower, _)| [upper, lower]),
        );
    }
    let prefixes = options
        .preserve_prefix
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    restore_preserved(&mut environment, &original, &keys, &prefixes);
    if let Some(prefix) = options.path_prefix {
        anyhow::ensure!(
            prefix.is_absolute(),
            "workspace executable path prefix must be absolute"
        );
        let path = crate::kernel_bin_path::path_leading_with(
            &prefix,
            environment.get(OsStr::new("PATH")).map(OsString::as_os_str),
        )
        .map_err(|_| anyhow::anyhow!("invalid workspace executable path prefix"))?;
        environment.insert("PATH".into(), path);
    }
    if options.print_environment {
        let environment = environment
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    key.into_string().map_err(|_| {
                        anyhow::anyhow!("workspace shell environment requires UTF-8")
                    })?,
                    value.into_string().map_err(|_| {
                        anyhow::anyhow!("workspace shell environment requires UTF-8")
                    })?,
                ))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        serde_json::to_writer(std::io::stdout().lock(), &environment)?;
        return Ok(());
    }
    let (program, arguments) = options.target.split_first().expect("required target argv");
    let error = Command::new(program)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .exec();
    Err(anyhow::anyhow!(
        "workspace target execution failed ({:?})",
        error.kind()
    ))
}

/// Restore owner-declared inputs, including absence. Project values cannot create
/// a provider/control variable which was absent from the admitted environment.
pub(crate) fn restore_preserved(
    environment: &mut BTreeMap<OsString, OsString>,
    original: &BTreeMap<OsString, OsString>,
    keys: &[&str],
    prefixes: &[&str],
) {
    let preserved = |key: &OsStr| {
        keys.iter().any(|name| key == OsStr::new(name))
            || prefixes
                .iter()
                .any(|prefix| key.as_encoded_bytes().starts_with(prefix.as_bytes()))
    };
    environment.retain(|key, _| !preserved(key));
    environment.extend(
        original
            .iter()
            .filter(|(key, _)| preserved(key))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
}

/// Render only source and owner policy metadata around an existing shell command.
/// The terminal owner remains responsible for launch admission and supplying runtime inputs.
pub(crate) fn wrap_shell_command(
    executable: &Path,
    source: &Path,
    keys: impl IntoIterator<Item = String>,
    prefixes: &[&str],
    preserve_selected_proxies: bool,
    command: &str,
) -> crate::error::Result<String> {
    let bin_dir = executable.parent().ok_or_else(|| {
        crate::error::CalmError::BadRequest("kernel executable has no parent directory".into())
    })?;
    let mut arguments = vec![
        executable.as_os_str().to_owned(),
        MODE.into(),
        "--source".into(),
        source.as_os_str().to_owned(),
    ];
    if let Some(config) = std::env::var_os("DIRENV_CONFIG") {
        arguments.extend(["--direnv-config".into(), config]);
    }
    for key in keys.into_iter().collect::<BTreeSet<_>>() {
        arguments.extend(["--preserve-key".into(), key.into()]);
    }
    for prefix in prefixes {
        arguments.extend(["--preserve-prefix".into(), (*prefix).into()]);
    }
    if preserve_selected_proxies {
        arguments.push("--preserve-selected-proxies".into());
    }
    arguments.extend([
        "--path-prefix".into(),
        bin_dir.as_os_str().to_owned(),
        "--".into(),
        "/bin/sh".into(),
        "-c".into(),
        command.into(),
    ]);
    arguments
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map(|value| crate::shell_command::sh_single_quote(&value))
                .map_err(|_| {
                    crate::error::CalmError::BadRequest(
                        "workspace execution arguments require UTF-8".into(),
                    )
                })
        })
        .collect::<crate::error::Result<Vec<_>>>()
        .map(|arguments| format!("exec {}", arguments.join(" ")))
}
