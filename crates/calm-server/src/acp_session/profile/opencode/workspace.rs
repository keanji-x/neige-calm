//! OpenCode v2.1.3's public local plugin and shell/create.before contract.
use crate::acp_session::environment::ShellEnvironment;
use crate::error::{CalmError, Result};
use crate::workspace_service::environment::SourceMapping;
use std::path::Path;

pub(super) fn prepare(root: &Path, mapping: SourceMapping) -> Result<ShellEnvironment> {
    let text = |path: &Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            CalmError::BadRequest("workspace shell metadata requires UTF-8 paths".into())
        })
    };
    std::fs::create_dir_all(root)?;
    let root = std::fs::canonicalize(root)?;
    let directory = tempfile::Builder::new()
        .prefix("opencode-workspace-")
        .tempdir_in(root)?;
    std::fs::write(
        directory.path().join("package.json"),
        r#"{"type":"module","main":"index.js"}"#,
    )?;
    std::fs::write(
        directory.path().join("index.js"),
        include_str!("workspace.js"),
    )?;
    let mut args = vec![
        "--workspace-environment-exec".to_owned(),
        "--print-environment".into(),
        "--source".into(),
        text(&mapping.source)?,
    ];
    if let Some(config) = std::env::var_os("DIRENV_CONFIG") {
        args.extend(["--direnv-config".into(), text(Path::new(&config))?]);
    }
    for root in mapping.derived_roots {
        args.extend(["--derived-root".into(), text(&root)?]);
    }
    for key in ["TERM", "OPENCODE_TERMINAL"] {
        args.extend(["--preserve-key".into(), key.into()]);
    }
    args.extend([
        "--preserve-prefix".into(),
        "NEIGE_".into(),
        "--path-prefix".into(),
        text(&crate::kernel_bin_path::kernel_bin_dir()?)?,
    ]);
    let mut config: serde_json::Value = serde_json::from_str(super::ASK_PERMISSION.1)?;
    config["plugins"] = serde_json::json!([{
        "package": text(directory.path())?,
        "options": {
            "executable": text(&std::env::current_exe()?)?,
            "args": args,
            "ready": text(&directory.path().join("ready"))?,
        },
    }]);
    Ok(ShellEnvironment {
        directory,
        config: ("OPENCODE_CONFIG_CONTENT", config.to_string().into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_workspace_activation_proof_is_unique_and_removed_with_its_process() {
        std::fs::create_dir_all("_local/tmp").unwrap();
        let root = tempfile::tempdir_in("_local/tmp").unwrap();
        let mapping = || SourceMapping {
            source: std::fs::canonicalize(root.path()).unwrap(),
            derived_roots: vec![],
        };
        let first = prepare(root.path(), mapping()).unwrap();
        let second = prepare(root.path(), mapping()).unwrap();
        assert!(matches!(first.check_ready(), Err(CalmError::Conflict(_))));
        std::fs::write(first.directory.path().join("ready"), "ready").unwrap();
        assert!(first.check_ready().is_ok());
        assert!(matches!(second.check_ready(), Err(CalmError::Conflict(_))));
        let previous = first.directory.path().to_owned();
        assert_ne!(previous, second.directory.path());
        drop(first);
        assert!(!previous.exists());
    }
}
