//! External REST/gh responses and crash fences; recovery policy lives in the plugin.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn write_shim(dir: &Path) {
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let path = dir.join("gh");
    std::fs::write(&path, include_str!("issue_api.sh")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
