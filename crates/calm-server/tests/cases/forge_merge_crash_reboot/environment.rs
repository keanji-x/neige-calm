use std::{ffi::OsString, path::Path};

use crate::support::forge_env::EnvGuard;

pub(super) fn prepare(root: &Path) -> (Vec<(&'static str, OsString)>, Vec<EnvGuard>) {
    let config = root.join("direnv-config");
    std::fs::create_dir_all(config.join("gh-config")).unwrap();
    // The fixture owns this parent directory, outside every candidate checkout.
    // Both kernel boots and in-process preparation use the same private trust store.
    std::fs::write(
        root.join(".envrc"),
        "export GH_TOKEN=synthetic-merge-crash-project\n\
         export GH_CONFIG_DIR=\"${DIRENV_CONFIG}/gh-config\"\n\
         PATH_add \"${DIRENV_CONFIG}/../shim-bin\"\n",
    )
    .unwrap();
    let environment = vec![
        ("DIRENV_CONFIG", config.into_os_string()),
        ("XDG_DATA_HOME", root.join("direnv-data").into_os_string()),
    ];
    let guards = environment
        .iter()
        .map(|(key, value)| EnvGuard::set(key, value))
        .collect();
    let allowed = std::process::Command::new("direnv")
        .arg("allow")
        .arg(root)
        .output()
        .expect("direnv must be installed for project environment tests");
    assert!(
        allowed.status.success(),
        "authorize fixture project environment"
    );
    (environment, guards)
}
