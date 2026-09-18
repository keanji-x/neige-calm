use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct TailnetConfig {
    pub binary: PathBuf,
    pub state_dir: PathBuf,
    pub state_dir_inherits_data: bool,
    pub hostname: String,
    pub enrollment_config: Option<PathBuf>,
}
impl TailnetConfig {
    pub fn defaults(release: &Path, data: &Path) -> Self {
        Self {
            binary: release.join("bin/neige-tailnet"),
            state_dir: data.join("tailnet"),
            state_dir_inherits_data: true,
            hostname: "neige".into(),
            enrollment_config: None,
        }
    }
    pub fn socket(&self) -> PathBuf {
        self.state_dir.join("app.sock")
    }
    pub fn helper_socket(&self) -> PathBuf {
        self.state_dir.join("helper.sock")
    }
    pub fn ingress_socket(&self) -> PathBuf {
        self.state_dir.join("ingress.sock")
    }
    pub fn ingress_config(&self) -> PathBuf {
        self.state_dir.join("ingress.json")
    }
    pub fn validate(&self, child_args: &[String]) -> anyhow::Result<()> {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "Private Tailnet currently requires Linux"
        );
        anyhow::ensure!(
            self.binary.is_absolute() && self.state_dir.is_absolute(),
            "Tailnet paths must be absolute"
        );
        anyhow::ensure!(
            self.enrollment_config
                .as_ref()
                .is_none_or(|p| p.is_absolute()),
            "Enrollment configuration path must be absolute"
        );
        anyhow::ensure!(
            self.socket().as_os_str().len() < 104
                && self.helper_socket().as_os_str().len() < 104
                && self.ingress_socket().as_os_str().len() < 104,
            "Tailnet state directory is too long for Unix sockets; configure a shorter private path"
        );
        anyhow::ensure!(
            !self.hostname.is_empty()
                && self.hostname.len() <= 63
                && self
                    .hostname
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid Tailnet hostname"
        );
        self.validate_provider_conflicts(child_args)
    }
    pub fn validate_provider_conflicts(&self, child_args: &[String]) -> anyhow::Result<()> {
        anyhow::ensure!(
            !child_args.iter().any(|a| a == "--mobile-access-config"
                || a.starts_with("--mobile-access-config=")
                || a == "--private-tailnet-config"
                || a.starts_with("--private-tailnet-config=")
                || a == "--private-tailnet-unavailable"),
            "Select exactly one mobile ingress provider; private-tailnet conflicts with child mobile configuration"
        );
        Ok(())
    }
}
