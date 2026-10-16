//! Filesystem path contracts shared by the runtime owner and host installer.

use std::path::{Path, PathBuf};

/// The shared Codex app-server's listener, isolated under one instance's data.
pub fn shared_codex_appserver_socket(data_dir: &Path) -> PathBuf {
    data_dir.join("run/codex-appserver.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_codex_socket_stays_under_instance_data() {
        assert_eq!(
            shared_codex_appserver_socket(Path::new("/instance/data")),
            Path::new("/instance/data/run/codex-appserver.sock")
        );
    }
}
